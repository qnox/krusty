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
                self.array_get(receiver, *index)
            }
            IrIntrinsic::ArraySet => {
                let (Some(receiver), [index, value]) = (receiver, args) else {
                    return Err("an array store with malformed operands".into());
                };
                self.array_set(receiver, *index, *value)
            }
            IrIntrinsic::ArraySize => {
                let (Some(receiver), []) = (receiver, args) else {
                    return Err("a malformed array length read".into());
                };
                self.array_size(receiver)
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
            IrIntrinsic::UnsignedToString { source } => {
                let (Some(receiver), []) = (receiver, args) else {
                    return Err("an unsigned string conversion with malformed operands".into());
                };
                self.unsigned_string_of(receiver, source)
            }
            // `Any?.toString()` is the string a template makes of its operand.
            IrIntrinsic::NullableAnyToString => {
                let value = match (receiver, args) {
                    (None, [value]) => *value,
                    (Some(value), []) => value,
                    _ => return Err("a malformed `toString` call".to_string()),
                };
                self.string_of(value)
            }
            IrIntrinsic::GeneratedPropertyHash { ty } => {
                let (None, [value]) = (receiver, args) else {
                    return Err("a malformed generated property hash".to_string());
                };
                self.generated_property_hash(*value, ty)
            }
            IrIntrinsic::GeneratedPropertyEquals { ty } => {
                let (None, [left, right]) = (receiver, args) else {
                    return Err("a malformed generated property comparison".to_string());
                };
                self.generated_property_equals(*left, *right, ty)
            }
            IrIntrinsic::DataClassArrayToString { ty } => {
                let (None, [value]) = (receiver, args) else {
                    return Err("a malformed generated array rendering".to_string());
                };
                self.generated_array_to_string(*value, ty)
            }
            other => Err(format!("a call to the intrinsic {other:?}")),
        }
    }

    /// Whether `target` is a string's `length`, by its getter's provider realization.
    pub(super) fn is_string_length_property(
        &self,
        target: krusty::fir::ExternalPropertyId,
    ) -> bool {
        self.callables
            .property(target)
            .and_then(|property| self.callables.callable(property.getter))
            .is_some_and(|getter| {
                getter.compiler_operation()
                    == Some(krusty::backend::BackendCompilerIntrinsic::StringLength)
            })
    }

    /// Leave the selected String receiver's UTF-16 storage array on the stack.
    fn string_storage(&mut self, receiver: u32) -> Result<(), Unsupported> {
        self.value(receiver, Ty::String)?;
        self.code
            .ref_as_non_null()
            .struct_get(self.runtime.string, 0);
        Ok(())
    }

    /// `a.compareTo(b)` on two integral or floating values: `-1`, `0` or `1`. A floating value is
    /// ordered as `Float.compare`/`Double.compare` order it: a total order with `-0.0` below `0.0`
    /// and every `NaN` equal to and above all else.
    fn compare(&mut self, operand: Ty, lhs: u32, rhs: u32) -> Result<(), Unsupported> {
        ordinary_numeric_carrier(operand)?;
        let key = crate::builtin_order::order_key(operand)
            .ok_or_else(|| format!("`compareTo` on `{}`", describe(operand)))?;
        let left = self.local(key);
        let right = self.local(key);
        for (operand_id, local) in [(lhs, left), (rhs, right)] {
            self.value(operand_id, operand)?;
            crate::builtin_order::store_order_key(
                &mut self.code,
                &self.runtime.hashing,
                operand,
                local,
            );
        }
        crate::builtin_order::emit_key_order(&mut self.code, key, left, right);
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
