//! Descriptor-owned typed virtual operations used by the Native runtime.

use super::*;

/// The thunks through which the runtime WALKS an object of a class of this file: its own
/// `iterator`, `hasNext` and `next`, each behind a fixed signature this side can call.
///
/// See the note on `KType::walk_iterator` for why a thunk rather than a vtable slot: an emitted
/// method has the signature its DECLARATION states, and neither `hasNext`'s machine `Boolean` nor
/// an `Iterator<Int>`'s unboxed element is something the runtime could read from a slot number.
#[derive(Clone, Copy, Default)]
pub(in super::super) struct WalkMembers {
    pub(in super::super) iterator: Option<FuncId>,
    pub(in super::super) has_next: Option<FuncId>,
    pub(in super::super) next: Option<FuncId>,
    /// For a class implementing `kotlin.CharSequence`, which is walked by its LENGTH and its
    /// indexed read: Kotlin's `CharSequence` declares no iterator at all.
    pub(in super::super) length: Option<FuncId>,
    pub(in super::super) char_at: Option<FuncId>,
    /// Selected Collection.size getter, independent of iterator inheritance.
    pub(in super::super) collection_size: Option<FuncId>,
}

/// The same, as the SLOTS the declaration pass reads out of the override edges, before the thunks
/// that dispatch to them are emitted.
#[derive(Clone, Copy, Default)]
pub(in super::super) struct WalkSlots {
    /// Each PLUS ONE, so that 0 says the class declares none — slot 0 is `kotlin.Any.equals`.
    pub(in super::super) iterator: u32,
    pub(in super::super) has_next: u32,
    pub(in super::super) next: u32,
    pub(in super::super) length: u32,
    pub(in super::super) char_at: u32,
}

impl WalkSlots {
    /// Whether the class declares any of the three, so a subclass knows whether to inherit.
    fn is_empty(self) -> bool {
        self.iterator == 0 && self.has_next == 0 && self.next == 0 && self.length == 0
    }
}

impl FileLowering<'_> {
    /// The three thunks through which the runtime walks an object of this class, emitted here; see
    /// [`WalkMembers`].
    ///
    /// Each dispatches VIRTUALLY on the slot [`Self::walk_slots`] found, so one thunk serves the
    /// whole subtree below the class that declares the member — a subclass overriding it is
    /// reached through the same thunk, which is also why a subclass inherits the pointer rather
    /// than needing one of its own.
    pub(super) fn define_walk_members(
        &mut self,
        class: ClassId,
        base: &str,
    ) -> Result<WalkMembers, Unsupported> {
        let slots = self.walk_slots(class);
        let mut members = WalkMembers::default();
        for (biased, name, answers) in [
            (slots.iterator, "iterator", any()),
            (slots.has_next, "hasNext", Ty::Boolean),
            (slots.next, "next", any()),
            (slots.length, "length", Ty::Int),
        ] {
            if biased == 0 {
                continue;
            }
            let slot = biased - 1;
            // What the SLOT answers, which is not what the thunk does: `next` on an
            // `Iterator<Int>` answers an unboxed machine integer and the runtime reads a
            // reference, so the conversion is the ordinary boundary one. Read from the slot
            // rather than from the member's declaration, because the slot is what the dispatch
            // lands on and the two need not agree — an entry standing in for a base whose
            // signature has another representation wears the BASE's.
            let declared = self.walk_slot_result(class, slot)?;
            let thunk =
                self.declare_local_function(&format!("{base}_walk_{name}"), &[any()], answers)?;
            let signature = self.signature_of(&[any()], answers)?;
            let label = format!("{base}_walk_{name}");
            self.emit_function(thunk, signature, answers, &label, &mut |body, params| {
                let Some(produced) = body.dispatch(params[0], slot, &[], declared, &[])? else {
                    return Err(declined!("a `Unit` answer from `{label}`"));
                };
                let Some(value) = body.convert(produced, Some(declared), answers)? else {
                    return Err(declined!("a `Unit` answer from `{label}`"));
                };
                body.builder.ins().return_(&[value]);
                body.terminate();
                Ok(())
            })?;
            match name {
                "iterator" => members.iterator = Some(thunk),
                "hasNext" => members.has_next = Some(thunk),
                "length" => members.length = Some(thunk),
                _ => members.next = Some(thunk),
            }
        }
        // The indexed read takes an ARGUMENT, which is the whole of what separates it from the
        // four above: the index crosses unboxed, at the width the declaration states.
        if slots.char_at != 0 {
            let slot = slots.char_at - 1;
            let declared = self.walk_slot_result(class, slot)?;
            let name = format!("{base}_walk_get");
            let thunk = self.declare_local_function(&name, &[any(), Ty::Int], Ty::Char)?;
            let signature = self.signature_of(&[any(), Ty::Int], Ty::Char)?;
            let parameter = self.walk_slot_parameter(class, slot)?;
            self.emit_function(thunk, signature, Ty::Char, &name, &mut |body, params| {
                let Some(index) = body.convert(params[1], Some(Ty::Int), parameter)? else {
                    return Err(declined!("a `Unit` index in `{name}`"));
                };
                let Some(produced) =
                    body.dispatch(params[0], slot, &[parameter], declared, &[index])?
                else {
                    return Err(declined!("a `Unit` answer from `{name}`"));
                };
                let Some(value) = body.convert(produced, Some(declared), Ty::Char)? else {
                    return Err(declined!("a `Unit` answer from `{name}`"));
                };
                body.builder.ins().return_(&[value]);
                body.terminate();
                Ok(())
            })?;
            members.char_at = Some(thunk);
        }
        members.collection_size = self.define_collection_size(class, base)?;
        Ok(members)
    }

    /// The type the entry in `slot` takes as its ONE parameter, for the thunk that dispatches
    /// through it to convert the operand to.
    fn walk_slot_parameter(&self, class: ClassId, slot: u32) -> Result<Ty, Unsupported> {
        let entry = self
            .model
            .layout(class)
            .vtable
            .get(slot as usize)
            .cloned()
            .ok_or_else(|| format!("a walk through slot {slot}, which no table has"))?;
        let fid = match entry {
            // A value class's own member wears its own signature behind the bridge that unboxes
            // the receiver.
            Slot::Function(fid) | Slot::ValueBridge { function: fid, .. } => fid,
            Slot::Bridge { declared, .. } => declared,
            other => return Err(declined!("a walk through the vtable entry {other:?}")),
        };
        match self.ir.functions[fid as usize].params.as_slice() {
            [parameter] => Ok(*parameter),
            _ => Err(declined!(
                "a walk through `{}`, which takes no one operand",
                self.ir.functions[fid as usize].name
            )),
        }
    }

    /// What the entry in `slot` of this class's table ANSWERS, for the thunk that dispatches
    /// through it to convert from.
    fn walk_slot_result(&self, class: ClassId, slot: u32) -> Result<Ty, Unsupported> {
        let entry = self
            .model
            .layout(class)
            .vtable
            .get(slot as usize)
            .cloned()
            .ok_or_else(|| format!("a walk through slot {slot}, which no table has"))?;
        match entry {
            Slot::Function(fid) | Slot::ValueBridge { function: fid, .. } => {
                Ok(self.ir.functions[fid as usize].ret)
            }
            // A stand-in for a base whose signature has another representation wears that base's,
            // which is what a caller reading the slot gets.
            Slot::Bridge { declared, .. } => Ok(self.ir.functions[declared as usize].ret),
            other => Err(declined!("a walk through the vtable entry {other:?}")),
        }
    }

    /// Where a class of this file keeps its own `iterator`, `hasNext` and `next`, for the runtime
    /// to walk an object of it; see [`WalkMembers`].
    ///
    /// Read from the OVERRIDE edges, the same source [`implemented_collections`] reads: what makes
    /// a class walkable is that a member of it ANSWERS for `kotlin.collections.Iterable` or
    /// `Iterator`, which is exactly what an edge to one of their declarations records — and it
    /// holds for a class reaching the type through another dependency type its supertype list does
    /// not name.
    ///
    /// A class declaring none of the three inherits its superclass's, because a slot number
    /// assigned at the declaring class is valid for every subclass and a subclass is walked
    /// through the same member. Inherited as a WHOLE rather than per member: a class that declares
    /// one of them declares the shape's answer, and mixing its number with a parent's for the
    /// other two would describe neither.
    pub(in super::super) fn walk_slots(&self, class: ClassId) -> WalkSlots {
        use crate::native::intrinsics::CollectionShape;
        let ir = self.ir;
        let declaration = &ir.classes[class as usize];
        // Which ROLE the class answers for, read from the edges; WHICH members to look up then
        // follows from the role rather than from the edges. An override whose result is the
        // interface's own type parameter records no edge of its own — `Iterator<T>.next(): T` is
        // exactly that shape — so a class answering `hasNext` would have been half-recorded, and
        // half a pair is no walk.
        let mut iterable = false;
        let mut iterator = false;
        let mut text = false;
        for edge in ir
            .function_overrides
            .get(&declaration.fq_name)
            .into_iter()
            .flatten()
        {
            if !matches!(
                edge.overridden,
                crate::fir::ResolvedFunctionOverrideTarget::External(_)
            ) {
                continue;
            }
            match (
                self.collection_shape(edge.overridden_owner),
                edge.name.as_str(),
            ) {
                (Some(CollectionShape::Iterable), "iterator") => iterable = true,
                // A sequence is walked exactly as an iterable is: its one representation member
                // is `iterator`, and the descriptor thunk dispatches to it the same way.
                (Some(CollectionShape::Sequence), "iterator") => iterable = true,
                (Some(CollectionShape::Iterator), "hasNext" | "next") => iterator = true,
                (Some(CollectionShape::Text), "get" | "subSequence") => text = true,
                _ => {}
            }
        }
        // `length` is a PROPERTY, so it reaches the class through a property-override edge rather
        // than a function one — and a `CharSequence` implementor that overrode nothing else would
        // be missed without it.
        for edge in ir
            .property_overrides
            .get(&declaration.fq_name)
            .into_iter()
            .flatten()
        {
            if matches!(
                edge.overridden,
                crate::fir::ResolvedPropertyOverrideTarget::External(_)
            ) && matches!(
                self.collection_shape(edge.overridden_owner),
                Some(CollectionShape::Text)
            ) {
                text = true;
            }
        }
        let mut slots = WalkSlots::default();
        if iterable {
            slots.iterator = self.walk_slot(class, "iterator");
        }
        if iterator {
            slots.has_next = self.walk_slot(class, "hasNext");
            slots.next = self.walk_slot(class, "next");
            // Both or neither: a walk asks an iterator for `hasNext` AND `next`, and half a pair
            // would leave the other read as something this class is not.
            if slots.has_next == 0 || slots.next == 0 {
                slots.has_next = 0;
                slots.next = 0;
            }
        }
        if text {
            slots.length = self.walk_accessor_slot(class, "length");
            slots.char_at = self.walk_slot_of(class, "get", 1);
            // Both or neither, for the reason the iterator's pair is: text is walked by its
            // length AND its indexed read, and half of that is no walk.
            if slots.length == 0 || slots.char_at == 0 {
                slots.length = 0;
                slots.char_at = 0;
            }
        }
        if slots.is_empty() {
            if let Some(parent) = self.model.layout(class).superclass {
                return self.walk_slots(parent);
            }
        }
        slots
    }

    /// The slot of a class's own `name` PROPERTY GETTER, PLUS ONE so that 0 says it has none.
    ///
    /// The runtime walk contract selects the source property role here; once found, dispatch uses
    /// the declaration's stable identity. A base's accessor need not be a method at all because a
    /// field-backed property's accessors are synthesized.
    fn walk_accessor_slot(&self, class: ClassId, name: &str) -> u32 {
        let mut at = Some(class);
        while let Some(id) = at {
            if let Some((index, _)) = self.ir.classes[id as usize]
                .properties
                .iter()
                .enumerate()
                .find(|(_, property)| property.name == name)
            {
                if let Some(target) =
                    crate::native::classes::local_property_target(self.ir, id, index)
                {
                    let key = crate::native::classes::SlotKey::Getter(target);
                    return self.model.slot(class, &key).map_or(0, |slot| slot + 1);
                }
            }
            at = self.model.layout(id).superclass;
        }
        0
    }

    /// The slot of a class's own nullary `name`, PLUS ONE so that 0 says it has none; see
    /// [`WalkSlots`].
    ///
    /// Searched up the super chain, because a class answering for the role may inherit the member
    /// from a base of this file rather than declare it — and a slot number assigned at the
    /// declaring class is valid for every subclass.
    fn walk_slot(&self, class: ClassId, name: &str) -> u32 {
        self.walk_slot_of(class, name, 0)
    }

    /// The same for a member of the given ARITY.
    fn walk_slot_of(&self, class: ClassId, name: &str, arity: usize) -> u32 {
        let mut current = Some(class);
        while let Some(id) = current {
            if let Some((slot, _, _)) = self.member_slot(id, name, arity) {
                return slot + 1;
            }
            current = self.model.layout(id).superclass;
        }
        0
    }

    /// A Collection's selected property identity, never an Iterable shape or a getter spelling.
    /// Lookup stays on the original class's table so inherited getters still dispatch virtually.
    fn collection_size_slot(&self, class: ClassId) -> Result<Option<u32>, Unsupported> {
        use crate::types::{ClassifierRole, CollectionKind};

        let mut at = Some(class);
        while let Some(owner) = at {
            let mut selected = None;
            for edge in self
                .ir
                .property_overrides
                .get(&self.ir.classes[owner as usize].fq_name)
                .into_iter()
                .flatten()
            {
                if !matches!(
                    edge.overridden,
                    crate::fir::ResolvedPropertyOverrideTarget::External(_)
                ) || edge.name != "size"
                    || !matches!(
                        self.classifiers.classifier(edge.overridden_owner).and_then(|fact| fact.role),
                        Some(ClassifierRole::MappedCollection(collection))
                            if matches!(collection.kind,
                                CollectionKind::Collection | CollectionKind::List | CollectionKind::Set)
                    )
                {
                    continue;
                }
                if edge.declared_type != Ty::Int
                    || edge.applied_type != Ty::Int
                    || edge.implementation_type != Ty::Int
                    || edge.declared_receiver.is_some()
                    || edge.implementation_receiver.is_some()
                {
                    return Err("a Collection.size override with a malformed signature".into());
                }
                let key = crate::native::classes::SlotKey::Getter(edge.implementation);
                let slot = self
                    .model
                    .slot(class, &key)
                    .ok_or_else(|| declined!("a selected Collection.size getter without a slot"))?;
                if selected.is_some_and(|previous| previous != slot) {
                    return Err("conflicting selected Collection.size getter slots".into());
                }
                selected = Some(slot);
            }
            if selected.is_some() {
                return Ok(selected);
            }
            at = self.model.layout(owner).superclass;
        }
        Ok(None)
    }

    fn define_collection_size(
        &mut self,
        class: ClassId,
        base: &str,
    ) -> Result<Option<FuncId>, Unsupported> {
        let Some(slot) = self.collection_size_slot(class)? else {
            return Ok(None);
        };
        // The edge's types and the physical slot must agree. A field getter and a bridge are
        // valid implementations too; the generic iterator-result reader cannot describe them.
        let entry = &self.model.layout(class).vtable[slot as usize];
        let owner = &self.ir.classes[class as usize];
        if matches!(entry, Slot::Abstract) && (owner.is_abstract || owner.is_interface) {
            // No instance can reach this descriptor's abstract member. A concrete subclass
            // resolves the inherited property identity against its own executable table.
            return Ok(None);
        }
        let exact = match entry {
            Slot::Function(function) => {
                let declaration = &self.ir.functions[*function as usize];
                declaration.params.is_empty() && declaration.ret == Ty::Int
            }
            Slot::FieldGetter { class, field } => {
                self.ir.classes[*class as usize].fields[*field as usize].ty == Ty::Int
            }
            Slot::AccessorBridge {
                declared,
                implemented,
                receiver,
                setter,
                ..
            } => *declared == Ty::Int && *implemented == Ty::Int && receiver.is_none() && !setter,
            _ => false,
        };
        if !exact {
            return Err("a Collection.size getter with a mismatched slot ABI".into());
        }
        let name = format!("{base}_collection_size");
        let thunk = self.declare_local_function(&name, &[any()], Ty::Int)?;
        let signature = self.signature_of(&[any()], Ty::Int)?;
        self.emit_function(thunk, signature, Ty::Int, &name, &mut |body, params| {
            let value = body
                .dispatch(params[0], slot, &[], Ty::Int, &[])?
                .ok_or_else(|| declined!("a Unit answer from Collection.size"))?;
            body.builder.ins().return_(&[value]);
            body.terminate();
            Ok(())
        })?;
        Ok(Some(thunk))
    }
}
