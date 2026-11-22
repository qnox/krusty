//! The `Map` members a class of the program publishes in its descriptor, for the runtime's map
//! entry points to answer for an object they did not make.
//!
//! A call through a `Map`-typed receiver reaches the runtime whichever unit made the object, and
//! only the object's own class knows its members. Like the walk thunks, each member is a thunk
//! with a fixed signature (`KMapMembers` in `krusty_rt.h`) that dispatches to the class's override
//! virtually, so one table serves the class and every subclass that inherits it.

use super::super::implementor_dispatch::ParameterBoundary;
use super::*;
use krusty::types::{ClassifierRole, CollectionKind};

/// One member of `KMapMembers`, in the table's order: how it is declared and what its thunk
/// takes and answers.
struct MapMember {
    name: &'static str,
    kind: MemberKind,
    /// The thunk's operands after the receiver, each a reference.
    operands: usize,
    answer: Ty,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberKind {
    Property,
    Function,
}

fn table() -> [MapMember; 8] {
    let member = |name, kind, operands, answer| MapMember {
        name,
        kind,
        operands,
        answer,
    };
    [
        member("size", MemberKind::Property, 0, Ty::Int),
        member("isEmpty", MemberKind::Function, 0, Ty::Boolean),
        member("containsKey", MemberKind::Function, 1, Ty::Boolean),
        member("containsValue", MemberKind::Function, 1, Ty::Boolean),
        member("get", MemberKind::Function, 1, any()),
        member("keys", MemberKind::Property, 0, any()),
        member("values", MemberKind::Property, 0, any()),
        member("entries", MemberKind::Property, 0, any()),
    ]
}

/// Where a class keeps one member: its slot, and the types the slot's entry takes and answers
/// beside the ones the `Map` declaration states.
struct MemberSlot {
    slot: u32,
    declared: (Vec<Ty>, Ty),
    implemented: (Vec<Ty>, Ty),
    /// Whether the overridden declaration has a collection special bridge: its erased operand is
    /// checked against the override's type, and the bridge's neutral result answers instead of
    /// the override when it is of another type. The ordinary implementor dispatch decides this
    /// by the same classifier role and member.
    bridge: bool,
}

impl FileLowering<'_> {
    /// The `Map` member table of `class`'s descriptor, emitted here, or `None` for a class that
    /// does not implement `Map` or does not override every member here (one inheriting them from
    /// a library class): the runtime then fails loudly on such a map, naming it.
    pub(super) fn define_map_members(
        &mut self,
        class: ClassId,
        base: &str,
    ) -> Result<Option<DataId>, Unsupported> {
        let declaration = &self.ir.classes[class as usize];
        if declaration.is_interface || declaration.is_abstract {
            return Ok(None);
        }
        let members = table();
        let mut slots = Vec::with_capacity(members.len());
        for member in &members {
            let Some(slot) = self.map_member_slot(class, member)? else {
                return Ok(None);
            };
            slots.push(slot);
        }
        let mut thunks = Vec::with_capacity(members.len());
        for (member, slot) in members.iter().zip(&slots) {
            thunks.push(self.define_map_member_thunk(base, member, slot)?);
        }
        let name = format!("kt_map_members_{base}");
        let id = self.declare_local_data(&name, false)?;
        let mut description = DataDescription::new();
        description.define(vec![0; thunks.len() * 8].into_boxed_slice());
        description.set_align(8);
        for (index, thunk) in thunks.into_iter().enumerate() {
            let func_ref = self.module.declare_func_in_data(thunk, &mut description);
            description.write_function_addr(index as u32 * 8, func_ref);
        }
        self.module
            .define_data(id, &description)
            .map_err(|error| format!("defining `{name}` ({error})"))?;
        Ok(Some(id))
    }

    /// The thunk for one member: the receiver and each operand as references, converted to what
    /// the declaration takes and on to what the class's entry takes, and the entry's answer
    /// converted back the same way.
    fn define_map_member_thunk(
        &mut self,
        base: &str,
        member: &MapMember,
        slot: &MemberSlot,
    ) -> Result<FuncId, Unsupported> {
        let mut params = vec![any()];
        params.extend(std::iter::repeat_n(any(), member.operands));
        let name = format!("{base}_map_{}", member.name);
        let thunk = self.declare_local_function(&name, &params, member.answer)?;
        let signature = self.signature_of(&params, member.answer)?;
        let answer = member.answer;
        self.emit_function(thunk, signature, answer, &name, &mut |body, values| {
            if let (true, Some(&declared), Some(&implementation)) = (
                slot.bridge,
                slot.declared.0.first(),
                slot.implemented.0.first(),
            ) {
                let boundary = ParameterBoundary {
                    declared,
                    implementation,
                };
                if let Some((accepted, rejected)) =
                    body.bridge_argument_guard(values[1], &boundary)?
                {
                    body.continue_in(rejected);
                    body.builder.seal_block(rejected);
                    let Some(neutral) = body.bridge_rejection(answer) else {
                        return Err(declined!("a `Unit` answer from `{name}`"));
                    };
                    body.builder.ins().return_(&[neutral]);
                    body.continue_in(accepted);
                    body.builder.seal_block(accepted);
                }
            }
            let mut operands = Vec::with_capacity(slot.implemented.0.len());
            for ((&value, &declared), &implemented) in values[1..]
                .iter()
                .zip(&slot.declared.0)
                .zip(&slot.implemented.0)
            {
                let Some(crossing) = body.convert(value, Some(any()), declared)? else {
                    return Err(declined!("a `Unit` operand of `{name}`"));
                };
                let Some(converted) = body.convert(crossing, Some(declared), implemented)? else {
                    return Err(declined!("a `Unit` operand of `{name}`"));
                };
                operands.push(converted);
            }
            let Some(produced) = body.dispatch(
                values[0],
                slot.slot,
                &slot.implemented.0,
                slot.implemented.1,
                &operands,
            )?
            else {
                return Err(declined!("a `Unit` answer from `{name}`"));
            };
            let Some(crossing) =
                body.convert(produced, Some(slot.implemented.1), slot.declared.1)?
            else {
                return Err(declined!("a `Unit` answer from `{name}`"));
            };
            let Some(value) = body.convert(crossing, Some(slot.declared.1), answer)? else {
                return Err(declined!("a `Unit` answer from `{name}`"));
            };
            body.builder.ins().return_(&[value]);
            body.terminate();
            Ok(())
        })?;
        Ok(thunk)
    }

    /// Where `class` keeps `member`: the override edge of `class`, or of the nearest superclass of
    /// this file, that implements the `Map` declaration of that name. A property is overridden by
    /// a property, or, in a class lowered from a library's serialized body, by its getter
    /// function. `None` where none does.
    fn map_member_slot(
        &self,
        class: ClassId,
        member: &MapMember,
    ) -> Result<Option<MemberSlot>, Unsupported> {
        let mut at = Some(class);
        while let Some(owner) = at {
            let found = match member.kind {
                MemberKind::Property => match self.map_property_slot(class, owner, member)? {
                    Some(found) => Some(found),
                    None => self.map_function_slot(class, owner, member)?,
                },
                MemberKind::Function => self.map_function_slot(class, owner, member)?,
            };
            if found.is_some() {
                return Ok(found);
            }
            at = self.model.layout(owner).superclass;
        }
        Ok(None)
    }

    /// Whether `owner` is `Map` or `MutableMap`, by its classifier role.
    fn is_map_declaration(&self, owner: krusty::types::TypeName) -> bool {
        matches!(
            self.classifiers.classifier(owner).and_then(|fact| fact.role),
            Some(ClassifierRole::MappedCollection(collection))
                if collection.kind == CollectionKind::Map
        )
    }

    /// Whether the member `name` of the collection declaration `owner` has a special bridge.
    fn has_special_bridge(&self, owner: krusty::types::TypeName, name: &str) -> bool {
        matches!(
            self.classifiers.classifier(owner).and_then(|fact| fact.role),
            Some(ClassifierRole::MappedCollection(collection))
                if collection.has_special_bridge(name)
        )
    }

    fn map_property_slot(
        &self,
        class: ClassId,
        owner: ClassId,
        member: &MapMember,
    ) -> Result<Option<MemberSlot>, Unsupported> {
        let mut selected: Option<MemberSlot> = None;
        for edge in self
            .ir
            .property_overrides
            .get(&self.ir.classes[owner as usize].fq_name)
            .into_iter()
            .flatten()
        {
            if !matches!(
                edge.overridden,
                krusty::fir::ResolvedPropertyOverrideTarget::External(_)
            ) || &*edge.name != member.name
                || !self.is_map_declaration(edge.overridden_owner)
            {
                continue;
            }
            if edge.declared_receiver.is_some()
                || edge.implementation_receiver.is_some()
                || self.carrier(edge.declared_type) != self.carrier(member.answer)
            {
                return Err(declined!(
                    "a `Map.{}` override with a malformed signature",
                    member.name
                ));
            }
            let key = crate::classes::SlotKey::Getter(edge.implementation);
            let Some(slot) = self.model.slot(class, &key) else {
                return Ok(None);
            };
            if selected.as_ref().is_some_and(|known| known.slot != slot) {
                return Err(declined!("conflicting `Map.{}` getter slots", member.name));
            }
            selected = Some(MemberSlot {
                slot,
                declared: (Vec::new(), edge.declared_type),
                implemented: (Vec::new(), edge.implementation_type),
                bridge: false,
            });
        }
        Ok(selected)
    }

    /// Whether the override of `overridden`, a function named `name`, implements `member`: the
    /// function of that name, or the getter of the property of that name.
    fn overrides_map_member(
        &self,
        overridden: krusty::fir::ExternalCallableId,
        name: &str,
        member: &MapMember,
    ) -> bool {
        let getter_of = self
            .callables
            .callable(overridden)
            .and_then(|fact| fact.getter_of());
        match (member.kind, getter_of) {
            (MemberKind::Function, None) => name == member.name,
            (MemberKind::Property, Some(property)) => property == member.name,
            _ => false,
        }
    }

    fn map_function_slot(
        &self,
        class: ClassId,
        owner: ClassId,
        member: &MapMember,
    ) -> Result<Option<MemberSlot>, Unsupported> {
        let ir = self.ir;
        let mut selected: Option<MemberSlot> = None;
        for edge in ir
            .function_overrides
            .get(&ir.classes[owner as usize].fq_name)
            .into_iter()
            .flatten()
        {
            let krusty::fir::ResolvedFunctionOverrideTarget::External(overridden) = edge.overridden
            else {
                continue;
            };
            if !self.is_map_declaration(edge.overridden_owner)
                || edge.declared_parameters.len() != member.operands
                || !self.overrides_map_member(overridden, &edge.name, member)
            {
                continue;
            }
            if self.carrier(edge.declared_result) != self.carrier(member.answer)
                || edge
                    .declared_parameters
                    .iter()
                    .any(|&parameter| self.carrier(parameter) != self.carrier(any()))
            {
                return Err(declined!(
                    "a `Map.{}` override with a malformed signature",
                    member.name
                ));
            }
            let Some(function) = edge.implementation_function.or_else(|| {
                let krusty::fir::ResolvedFunctionOverrideTarget::Module(callable) =
                    edge.implementation
                else {
                    return None;
                };
                ir.checked_callable_functions.get(&callable).copied()
            }) else {
                return Ok(None);
            };
            let Some(implementation_owner) = ir.class_id_by_name(edge.implementation_owner) else {
                return Ok(None);
            };
            let key = crate::classes::function_key(ir, implementation_owner, function);
            let Some(slot) = self.model.slot(class, &key) else {
                return Ok(None);
            };
            if selected.as_ref().is_some_and(|known| known.slot != slot) {
                return Err(declined!("conflicting `Map.{}` slots", member.name));
            }
            let implementation = &ir.functions[function as usize];
            if implementation.params.len() != member.operands {
                return Err(declined!(
                    "a `Map.{}` override with a malformed signature",
                    member.name
                ));
            }
            selected = Some(MemberSlot {
                slot,
                declared: (edge.declared_parameters.to_vec(), edge.declared_result),
                implemented: (implementation.params.clone(), implementation.ret),
                bridge: self.has_special_bridge(edge.overridden_owner, member.name),
            });
        }
        Ok(selected)
    }
}
