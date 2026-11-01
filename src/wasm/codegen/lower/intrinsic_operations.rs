//! Compiler-selected operations realized directly by Wasm lowering.
//!
//! Common IR supplies the selected operation identity and checked operands. This module owns only
//! their WasmGC representation; it never recovers a declaration from source spelling.

use super::*;

impl Body<'_> {
    pub(super) fn intrinsic(
        &mut self,
        operation: IrIntrinsic,
        receiver: Option<u32>,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        match operation {
            IrIntrinsic::ArrayGet => {
                let (Some(receiver), [index]) = (receiver, args) else {
                    return Err("an array read with malformed operands".into());
                };
                self.reference_array_get(receiver, *index)
            }
            IrIntrinsic::ArraySet => {
                let (Some(receiver), [index, value]) = (receiver, args) else {
                    return Err("an array write with malformed operands".into());
                };
                self.reference_array_set(receiver, *index, *value)
            }
            IrIntrinsic::ArraySize => {
                let (Some(receiver), []) = (receiver, args) else {
                    return Err("an array size read with malformed operands".into());
                };
                self.reference_array_size(receiver)
            }
            IrIntrinsic::PrimitiveCompare { operand, .. } => {
                let (lhs, rhs) = match (receiver, args) {
                    (None, [lhs, rhs]) => (*lhs, *rhs),
                    (Some(lhs), [rhs]) => (lhs, *rhs),
                    _ => return Err("`compareTo` with an unexpected operand shape".to_string()),
                };
                self.compare(operand, lhs, rhs)
            }
            IrIntrinsic::UnsignedCompare { operand } => {
                let (None, [lhs, rhs]) = (receiver, args) else {
                    return Err("an unsigned comparison with unexpected operands".to_string());
                };
                self.unsigned_compare(operand, *lhs, *rhs)
            }
            IrIntrinsic::StringLength => {
                let (Some(receiver), []) = (receiver, args) else {
                    return Err("a malformed `String.length` read".to_string());
                };
                self.string_storage(receiver)?;
                self.code.array_len();
                Ok(())
            }
            IrIntrinsic::StringGet => {
                let (Some(receiver), [index]) = (receiver, args) else {
                    return Err("a malformed string read".to_string());
                };
                self.string_storage(receiver)?;
                self.value(*index, Ty::Int)?;
                self.code.array_get_u(self.runtime.chars);
                Ok(())
            }
            other => Err(format!("a call to the intrinsic {other:?}")),
        }
    }

    /// Leave the selected String receiver's UTF-16 storage array on the stack.
    fn string_storage(&mut self, receiver: u32) -> Result<(), Unsupported> {
        self.value(receiver, Ty::String)?;
        self.code
            .ref_as_non_null()
            .struct_get(self.runtime.string, 0);
        Ok(())
    }

    /// `a.compareTo(b)` on two integral values: `-1`, `0` or `1`. A floating-point `compareTo` is a
    /// total order (`NaN` greatest, `-0.0` below `0.0`), not this, and declines.
    fn compare(&mut self, operand: Ty, lhs: u32, rhs: u32) -> Result<(), Unsupported> {
        let value = ordinary_numeric_carrier(operand)?;
        let (greater, less) = match value {
            Some(ValType::I32) => (GT_S32, LT_S32),
            Some(ValType::I64) => (GT_S64, LT_S64),
            _ => return Err(format!("`compareTo` on `{}`", describe(operand))),
        };
        let value = value.expect("matched above");
        let left = self.local(value);
        let right = self.local(value);
        self.value(lhs, operand)?;
        self.code.local_set(left);
        self.value(rhs, operand)?;
        self.code.local_set(right);
        self.code
            .local_get(left)
            .local_get(right)
            .op(greater)
            .local_get(left)
            .local_get(right)
            .op(less)
            .op(SUB32);
        Ok(())
    }

    /// The normalized selected `UInt`/`ULong` ordering, as `-1`, `0`, or `1`.
    fn unsigned_compare(&mut self, operand: Ty, lhs: u32, rhs: u32) -> Result<(), Unsupported> {
        let (value, greater, less) = match operand {
            Ty::UInt => (ValType::I32, 0x4b, 0x49),
            Ty::ULong => (ValType::I64, 0x56, 0x54),
            _ => return Err(format!("an unsigned comparison of `{}`", describe(operand))),
        };
        let left = self.local(value);
        let right = self.local(value);
        self.value(lhs, operand)?;
        self.code.local_set(left);
        self.value(rhs, operand)?;
        self.code.local_set(right);
        self.code
            .local_get(left)
            .local_get(right)
            .op(greater)
            .local_get(left)
            .local_get(right)
            .op(less)
            .op(SUB32);
        Ok(())
    }
}
