//! Kotlin/Native's raw pointer operations (`NativePtr`, `CPointer`).
//!
//! The platform stdlib declares them as `@TypedIntrinsic` externals without a body, and the
//! provider authenticates each by its exact kind and signature
//! (`krusty::libraries::NativePointerOperation`). A pointer is an address in a machine word: the
//! address `0` is the null pointer, and the collector ignores a word that is not inside one of its
//! chunks, so a pointer can travel wherever a reference does without being traced.

use krusty::libraries::NativePointerOperation;

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// One authenticated pointer operation. `receiver` is the dispatch receiver of a member and
    /// `args` its arguments; the result is carried at `ret`.
    pub(super) fn native_pointer(
        &mut self,
        operation: NativePointerOperation,
        receiver: Option<u32>,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        match (operation, receiver, args) {
            (NativePointerOperation::Null, None, []) => {
                let zero = self.builder.ins().iconst(types::I64, 0);
                self.address_answer(zero, ret)
            }
            (NativePointerOperation::Reinterpret, None, [operand]) => {
                let Some(value) = self.address_operand(*operand)? else {
                    return Ok(None);
                };
                self.address_answer(value, ret)
            }
            (NativePointerOperation::ToLong, Some(pointer), []) => {
                let Some(value) = self.address_operand(pointer)? else {
                    return Ok(None);
                };
                self.address_answer(value, ret)
            }
            (NativePointerOperation::PlusLong, Some(pointer), [offset]) => {
                let Some(address) = self.address_operand(pointer)? else {
                    return Ok(None);
                };
                let Some(offset) = self.coerce(*offset, Ty::Long)? else {
                    return Err("a pointer offset of `Unit`".into());
                };
                if self.terminated {
                    return Ok(None);
                }
                let sum = self.builder.ins().iadd(address, offset);
                self.address_answer(sum, ret)
            }
            _ => Err(format!("a malformed `{operation:?}` pointer operation").into()),
        }
    }

    /// Whether `target` reads the address a pointer class wraps: its `value` property at the type
    /// the class's value declaration gives it, or `CPointer.rawValue`, which is that address as a
    /// `NativePtr`. A pointer is that address, so the read is the pointer itself.
    pub(super) fn is_address_value_read(&self, target: krusty::fir::ExternalPropertyId) -> bool {
        let Some(property) = self.file.callables.property(target) else {
            return false;
        };
        if !krusty::backend::address_carriers::is_address_class(property.owner) {
            return false;
        }
        match &*property.name {
            "value" => self.file.values.underlying(property.owner) == Some(property.result),
            "rawValue" => {
                property.result
                    == Ty::obj_name(krusty::types::type_name("kotlin/native/internal/NativePtr"))
            }
            _ => false,
        }
    }

    /// The address a pointer is, read through the property that exposes it.
    pub(super) fn address_value_read(
        &mut self,
        receiver: u32,
        result: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(address) = self.address_operand(receiver)? else {
            return Ok(None);
        };
        self.address_answer(address, result)
    }

    /// An operand's address. The operand is carried at its own checked type, which must be one
    /// machine word: the value of a pointer, or the `Long` of an offset already converted.
    fn address_operand(&mut self, operand: u32) -> Result<Option<Value>, Unsupported> {
        let Some(ty) = self.checked_type(operand) else {
            return Err("a pointer operand without a checked type".into());
        };
        let Some(value) = self.coerce(operand, ty)? else {
            return Err("a pointer operand of `Unit`".into());
        };
        if self.terminated {
            return Ok(None);
        }
        if self.carrier(ty).clif() != Some(types::I64) {
            return Err(format!("a pointer operand `{ty:?}` that is not one machine word").into());
        }
        Ok(Some(value))
    }

    /// `address` as the result type `ret`, which must be carried as one machine word too.
    fn address_answer(&mut self, address: Value, ret: Ty) -> Result<Option<Value>, Unsupported> {
        if self.carrier(ret).clif() != Some(types::I64) {
            return Err(format!("a pointer result `{ret:?}` that is not one machine word").into());
        }
        Ok(Some(address))
    }
}
