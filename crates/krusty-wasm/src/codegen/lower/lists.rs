//! Allocation of a runtime `ArrayList` and of the array wrapper it is filled from.

use super::super::super::encode::HeapType;
use super::super::super::interface_dispatch::MemberRef;
use super::super::super::lists::ListSlot;
use super::*;

use krusty::types::{Ty, TypeName};

impl Body<'_> {
    /// The file-local `ArrayAsCollection`: a private `Collection` whose constructor is an array
    /// and a varargs flag. Its source spelling is not its classifier identity, so the shape is
    /// what a construction recognizes. A source class of the same shape is left alone.
    pub(super) fn array_collection_wrapper(&self, internal: TypeName) -> bool {
        if !internal.contains("klib-local") {
            return false;
        }
        let Some(class) = self.ir.class_id_by_name(internal) else {
            return false;
        };
        let class = &self.ir.classes[class as usize];
        let [values, flag] = class.ctor_args.as_slice() else {
            return false;
        };
        if !values.ty.is_reference_array() || flag.ty != Ty::Boolean {
            return false;
        }
        let collection = krusty::types::wk::collection();
        class.interfaces.contains_name(collection)
            || class
                .supertypes
                .iter()
                .any(|ty| ty.obj_parts().is_some_and(|(name, _)| name == collection))
    }

    /// `ArrayAsCollection(array, isVarargs)`: keep the array. `isVarargs` only chooses whether a
    /// later copy has to rebox, and this wrapper is only built for a varargs array of references.
    pub(super) fn array_wrapper_construction(&mut self, args: &[u32]) -> Result<(), Unsupported> {
        let [array, flag] = *args else {
            return Err("an array wrapper with another arity".to_string());
        };
        self.value(array, objects::any())?;
        self.code
            .ref_cast(true, HeapType::Concrete(self.runtime.lists.array));
        let ty = self
            .type_of(flag)?
            .ok_or("an array wrapper flag without a type")?;
        self.value(flag, ty)?;
        if carrier(ty)?.is_some() {
            self.code.drop_();
        }
        self.runtime.lists.wrap_array(&mut self.code);
        Ok(())
    }

    /// `ArrayList(...)`. A capacity hint allocates an empty list. A collection copies the array
    /// it wraps.
    pub(super) fn array_list_construction(&mut self, args: &[u32]) -> Result<(), Unsupported> {
        match args {
            [] => {
                self.runtime.lists.construct_empty(&mut self.code);
                Ok(())
            }
            [arg] => {
                let ty = self
                    .type_of(*arg)?
                    .ok_or("an `ArrayList` constructor argument without a type")?;
                if carrier(ty)? == Some(ValType::I32) {
                    self.value(*arg, ty)?;
                    self.code.drop_();
                    self.runtime.lists.construct_empty(&mut self.code);
                } else {
                    self.value(*arg, objects::any())?;
                    self.runtime.lists.construct_from(&mut self.code);
                }
                Ok(())
            }
            _ => Err("an `ArrayList` constructor with another arity".to_string()),
        }
    }

    /// `List.get`, `Collection.isEmpty`, `Collection.size`, `Iterable.iterator`, or `MutableList.add`
    /// on the runtime list.
    pub(super) fn implement_list(
        &mut self,
        member: MemberRef,
        slot: ListSlot,
    ) -> Result<(), Unsupported> {
        let function = self.runtime.lists.function(slot);
        self.runtime.objects.interfaces.implement_representation(
            member,
            self.runtime.lists.list,
            function,
        )?;
        Ok(())
    }

    /// `Iterator.hasNext` or `Iterator.next` for a list walker.
    pub(super) fn implement_list_iterator(
        &mut self,
        member: MemberRef,
        has_next: bool,
    ) -> Result<(), Unsupported> {
        let function = self.runtime.lists.walk(has_next);
        self.runtime.objects.interfaces.implement_representation(
            member,
            self.runtime.lists.iterator,
            function,
        )?;
        Ok(())
    }
}
