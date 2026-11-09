//! The per-property operations of generated `equals` and `hashCode`.
//!
//! Common lowering builds a data class's (or a value class's) generated members and leaves each
//! property's comparison and hash as one selected operation over its declared type. This module
//! realizes them by the property's Wasm representation, under the rules `super::super::super::hashing`
//! states.

use krusty::backend::class_tables::AnyMember;
use krusty::types::TyKind;

use super::objects::any;
use super::*;

/// The non-null `Unit` type, in its scalar or its classifier spelling.
fn is_unit(ty: Ty) -> bool {
    ty == Ty::Unit || matches!(ty.kind(), TyKind::Obj(name, _) if name == krusty::types::wk::unit())
}

#[cfg(test)]
mod tests;

impl Body<'_> {
    /// Push the hash of property value `value`, declared of type `ty`.
    pub(super) fn generated_property_hash(
        &mut self,
        value: u32,
        ty: Ty,
    ) -> Result<(), Unsupported> {
        if ty.non_null().is_array() {
            // kotlinc hashes an array property by its CONTENTS, which the runtime does not
            // compute yet.
            return Err(format!("the content hash of a `{}` property", describe(ty)));
        }
        // `Unit` has one instance. Its hash is that instance's fixed identity hash, whether the
        // property is spelled as the scalar or as `kotlin.Unit`. Another class in the file may
        // still inherit `Any.hashCode`; that does not make this property's hash a virtual call.
        if is_unit(ty.non_null()) {
            self.statement(value)?;
            self.code.i32_const(crate::hashing::UNIT_HASH);
            return Ok(());
        }
        match carrier(ty)? {
            Some(carried) if carried == STRING => {
                self.value(value, ty)?;
                self.code.call(self.runtime.hashing.string_hash);
            }
            // An object or a nullable primitive's box, through its table. `Nothing` has no value.
            Some(ValType::Ref { .. }) | None => {
                if self.may_reach_any_default(ty, AnyMember::HashCode)? {
                    return Err("`kotlin.Any.hashCode`'s identity hash".to_string());
                }
                self.value(value, any())?;
                self.code.call(self.runtime.hashing.hash_code);
            }
            Some(_) => {
                self.value(value, ty)?;
                if !self.runtime.hashing.emit_scalar_hash(&mut self.code, ty) {
                    return Err(format!("the hash of a `{}` property", describe(ty)));
                }
            }
        }
        Ok(())
    }

    /// Push whether property values `left` and `right`, declared of type `ty`, are equal.
    pub(super) fn generated_property_equals(
        &mut self,
        left: u32,
        right: u32,
        ty: Ty,
    ) -> Result<(), Unsupported> {
        match carrier(ty)? {
            // `Unit` has one value, so two of them are equal once both have been evaluated.
            None => {
                self.statement(left)?;
                self.statement(right)?;
                self.code.i32_const(1);
            }
            Some(carried) if carried == STRING => {
                self.value(left, ty)?;
                self.value(right, ty)?;
                self.code.call(self.runtime.string_equals);
            }
            // Array wrappers keep identity equality; classes use their selected `equals` slot.
            Some(ValType::Ref { .. }) => self.object_equality(left, right)?,
            Some(ValType::F32 | ValType::F64) => {
                for operand in [left, right] {
                    self.value(operand, ty)?;
                    self.runtime.hashing.emit_float_bits(&mut self.code, ty);
                }
                self.code.op(if ty.non_null() == Ty::Float {
                    EQ32
                } else {
                    EQ64
                });
            }
            Some(ValType::I64) => {
                self.value(left, ty)?;
                self.value(right, ty)?;
                self.code.op(EQ64);
            }
            Some(_) => {
                self.value(left, ty)?;
                self.value(right, ty)?;
                self.code.op(EQ32);
            }
        }
        Ok(())
    }
}
