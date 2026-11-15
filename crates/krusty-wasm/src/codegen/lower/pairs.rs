//! Reads and allocation of the runtime's `kotlin.Pair`.

use super::super::super::encode::HeapType;
use super::super::super::objects::REFERENCE;
use super::super::super::pairs::{FIRST_FIELD, SECOND_FIELD};
use super::*;

impl Body<'_> {
    /// `Pair(first, second)`: an identity object whose two fields are the components.
    pub(super) fn pair_construction(&mut self, args: &[u32]) -> Result<(), Unsupported> {
        let [first, second] = *args else {
            return Err("a `Pair` constructor with another arity".to_string());
        };
        self.code
            .global_get(self.runtime.pairs.table)
            .call(self.runtime.identity.allocate);
        self.pair_component_value(first)?;
        self.pair_component_value(second)?;
        self.code.struct_new(self.runtime.pairs.object);
        Ok(())
    }

    /// One component of a pair, as a reference field. A scalar result is the unboxed value.
    pub(super) fn pair_field(
        &mut self,
        receiver: u32,
        field: u32,
        result: Ty,
    ) -> Result<(), Unsupported> {
        let receiver_ty = self
            .type_of(receiver)?
            .ok_or("a `Pair` component read without a receiver type")?;
        self.value(receiver, receiver_ty)?;
        self.code
            .ref_cast(false, HeapType::Concrete(self.runtime.pairs.object))
            .struct_get(self.runtime.pairs.object, field);
        self.finish_pair_field(result)
    }

    /// `first` or `second` on the runtime pair, when `target` is that property.
    pub(super) fn pair_property_field(
        &self,
        target: krusty::fir::ExternalPropertyId,
    ) -> Option<u32> {
        let property = self.callables.property(target)?;
        if !property.owner.matches("kotlin/Pair") {
            return None;
        }
        match property.name.as_ref() {
            "first" => Some(FIRST_FIELD),
            "second" => Some(SECOND_FIELD),
            _ => None,
        }
    }

    fn pair_component_value(&mut self, component: u32) -> Result<(), Unsupported> {
        let ty = self
            .type_of(component)?
            .ok_or("a `Pair` component without a type")?;
        self.value(component, ty)?;
        match carrier(ty)? {
            Some(ValType::Ref { .. }) => {}
            Some(_) => self.box_value(ty)?,
            None => {
                self.code.ref_null(HeapType::Eq);
            }
        }
        Ok(())
    }

    fn finish_pair_field(&mut self, result: Ty) -> Result<(), Unsupported> {
        match carrier(result)? {
            None => {
                self.code.drop_();
            }
            Some(wanted @ ValType::Ref { heap, nullable }) if wanted != REFERENCE => {
                self.code.ref_cast(nullable, heap);
            }
            Some(ValType::Ref { .. }) => {}
            Some(_) => self.unbox(result)?,
        }
        Ok(())
    }
}
