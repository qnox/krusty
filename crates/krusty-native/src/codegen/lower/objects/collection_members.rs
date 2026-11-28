//! The `Map` and `List` members a class of the program publishes in its descriptor, for the
//! runtime's map and list entry points to answer for an object they did not make.
//!
//! A call through a `Map`- or `List`-typed receiver reaches the runtime whichever unit made the
//! object, and only the object's own class knows its members: a library body's local class, such
//! as the view a primitive array's `asList()` answers, is laid out in a unit no call site can see.
//! Like the walk thunks, each member is a thunk with a fixed signature (`KMapMembers` and
//! `KListMembers` in `krusty_rt.h`) that dispatches to the class's override virtually, so one
//! table serves the class and every subclass that inherits it.

use super::super::implementor_dispatch::ParameterBoundary;
use super::*;
use krusty::types::{ClassifierRole, CollectionKind};

/// One of the member tables a descriptor carries.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MemberTable {
    /// `KMapMembers`: every member, or no table.
    Map,
    /// `KListMembers`: each member the class overrides, the others NULL, which the runtime
    /// reports by name when it is asked for one.
    List,
}

impl MemberTable {
    /// Its members, in the C table's order.
    fn members(self) -> Vec<CollectionMember> {
        let member = |name, kind, operands, answer| CollectionMember {
            name,
            kind,
            operands,
            answer,
        };
        match self {
            Self::Map => vec![
                member("size", MemberKind::Property, vec![], Ty::Int),
                member("isEmpty", MemberKind::Function, vec![], Ty::Boolean),
                member(
                    "containsKey",
                    MemberKind::Function,
                    vec![any()],
                    Ty::Boolean,
                ),
                member(
                    "containsValue",
                    MemberKind::Function,
                    vec![any()],
                    Ty::Boolean,
                ),
                member("get", MemberKind::Function, vec![any()], any()),
                member("keys", MemberKind::Property, vec![], any()),
                member("values", MemberKind::Property, vec![], any()),
                member("entries", MemberKind::Property, vec![], any()),
            ],
            Self::List => vec![
                member("get", MemberKind::Function, vec![Ty::Int], any()),
                member("indexOf", MemberKind::Function, vec![any()], Ty::Int),
                member("lastIndexOf", MemberKind::Function, vec![any()], Ty::Int),
                member("contains", MemberKind::Function, vec![any()], Ty::Boolean),
            ],
        }
    }

    /// Whether a table needs every member: the runtime's map entry points call any of them
    /// without asking first.
    fn complete(self) -> bool {
        self == Self::Map
    }

    /// The collection declarations whose members the table holds. `List` redeclares `contains`
    /// from `Collection`, and an override may name either.
    fn declares(self, kind: CollectionKind) -> bool {
        match self {
            Self::Map => kind == CollectionKind::Map,
            Self::List => matches!(kind, CollectionKind::List | CollectionKind::Collection),
        }
    }

    fn spelling(self) -> &'static str {
        match self {
            Self::Map => "map",
            Self::List => "list",
        }
    }
}

/// One member of a table: how it is declared and what its thunk takes and answers.
struct CollectionMember {
    name: &'static str,
    kind: MemberKind,
    /// The thunk's operands after the receiver, at the carriers the declaration erases them to.
    operands: Vec<Ty>,
    answer: Ty,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberKind {
    Property,
    Function,
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
    /// The `table` of `class`'s descriptor, emitted here, or `None` for a class that implements
    /// none of its members. A [complete](MemberTable::complete) table is also `None` for a class
    /// that does not override every member (one inheriting them from a library class): the runtime
    /// then fails loudly on such an object, naming it.
    pub(super) fn define_collection_members(
        &mut self,
        class: ClassId,
        base: &str,
        table: MemberTable,
    ) -> Result<Option<DataId>, Unsupported> {
        let declaration = &self.ir.classes[class as usize];
        if declaration.is_interface || declaration.is_abstract {
            return Ok(None);
        }
        let members = table.members();
        let mut slots = Vec::with_capacity(members.len());
        for member in &members {
            let slot = self.collection_member_slot(class, table, member)?;
            if slot.is_none() && table.complete() {
                return Ok(None);
            }
            slots.push(slot);
        }
        if slots.iter().all(Option::is_none) {
            return Ok(None);
        }
        let mut thunks = Vec::with_capacity(members.len());
        for (member, slot) in members.iter().zip(&slots) {
            thunks.push(
                slot.as_ref()
                    .map(|slot| self.define_collection_member_thunk(base, table, member, slot))
                    .transpose()?,
            );
        }
        let name = format!("kt_{}_members_{base}", table.spelling());
        let id = self.declare_local_data(&name, false)?;
        let mut description = DataDescription::new();
        description.define(vec![0; thunks.len() * 8].into_boxed_slice());
        description.set_align(8);
        for (index, thunk) in thunks.into_iter().enumerate() {
            let Some(thunk) = thunk else {
                continue;
            };
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
    fn define_collection_member_thunk(
        &mut self,
        base: &str,
        table: MemberTable,
        member: &CollectionMember,
        slot: &MemberSlot,
    ) -> Result<FuncId, Unsupported> {
        let mut params = vec![any()];
        params.extend(member.operands.iter().copied());
        let name = format!("{base}_{}_{}", table.spelling(), member.name);
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
            for (((&value, &carried), &declared), &implemented) in values[1..]
                .iter()
                .zip(&member.operands)
                .zip(&slot.declared.0)
                .zip(&slot.implemented.0)
            {
                let Some(crossing) = body.convert(value, Some(carried), declared)? else {
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
    /// this file, that implements the `table`'s declaration of that name. A property is overridden by
    /// a property, or, in a class lowered from a library's serialized body, by its getter
    /// function. `None` where none does.
    fn collection_member_slot(
        &self,
        class: ClassId,
        table: MemberTable,
        member: &CollectionMember,
    ) -> Result<Option<MemberSlot>, Unsupported> {
        let mut at = Some(class);
        while let Some(owner) = at {
            let found = match member.kind {
                MemberKind::Property => {
                    match self.collection_property_slot(class, owner, table, member)? {
                        Some(found) => Some(found),
                        None => self.collection_function_slot(class, owner, table, member)?,
                    }
                }
                MemberKind::Function => {
                    self.collection_function_slot(class, owner, table, member)?
                }
            };
            if found.is_some() {
                return Ok(found);
            }
            at = self.model.layout(owner).superclass;
        }
        Ok(None)
    }

    /// Whether `owner` is a declaration `table` holds members of (either face), by its
    /// classifier role.
    fn is_table_declaration(&self, owner: krusty::types::TypeName, table: MemberTable) -> bool {
        matches!(
            self.classifiers.classifier(owner).and_then(|fact| fact.role),
            Some(ClassifierRole::MappedCollection(collection)) if table.declares(collection.kind)
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

    fn collection_property_slot(
        &self,
        class: ClassId,
        owner: ClassId,
        table: MemberTable,
        member: &CollectionMember,
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
                || !self.is_table_declaration(edge.overridden_owner, table)
            {
                continue;
            }
            if edge.declared_receiver.is_some()
                || edge.implementation_receiver.is_some()
                || self.carrier(edge.declared_type) != self.carrier(member.answer)
            {
                return Err(declined!(
                    "a `{}.{}` override with a malformed signature",
                    edge.overridden_owner.render().replace('/', "."),
                    member.name
                ));
            }
            let key = crate::classes::SlotKey::Getter(edge.implementation);
            let Some(slot) = self.model.slot(class, &key) else {
                return Ok(None);
            };
            if selected.as_ref().is_some_and(|known| known.slot != slot) {
                return Err(declined!(
                    "conflicting `{}.{}` getter slots",
                    edge.overridden_owner.render().replace('/', "."),
                    member.name
                ));
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
    fn overrides_collection_member(
        &self,
        overridden: krusty::fir::ExternalCallableId,
        name: &str,
        member: &CollectionMember,
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

    fn collection_function_slot(
        &self,
        class: ClassId,
        owner: ClassId,
        table: MemberTable,
        member: &CollectionMember,
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
            if !self.is_table_declaration(edge.overridden_owner, table)
                || edge.declared_parameters.len() != member.operands.len()
                || !self.overrides_collection_member(overridden, &edge.name, member)
            {
                continue;
            }
            let owner_name = edge.overridden_owner.render().replace('/', ".");
            if self.carrier(edge.declared_result) != self.carrier(member.answer)
                || edge
                    .declared_parameters
                    .iter()
                    .zip(&member.operands)
                    .any(|(&parameter, &carried)| self.carrier(parameter) != self.carrier(carried))
            {
                return Err(declined!(
                    "a `{owner_name}.{}` override with a malformed signature",
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
                return Err(declined!(
                    "conflicting `{owner_name}.{}` slots",
                    member.name
                ));
            }
            let implementation = &ir.functions[function as usize];
            if implementation.params.len() != member.operands.len() {
                return Err(declined!(
                    "a `{owner_name}.{}` override with a malformed signature",
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
