//! Wasm realization of the operand of a parameter a call leaves to its default.
//!
//! The callee's default-argument entry overwrites the operand before anything reads it, and a
//! dependency's default entry does the same, so any value of the parameter's carrier serves. Its
//! zero is the one every carrier has.

use super::*;
use crate::wasm::encode::HeapType;

impl Body<'_> {
    /// An [`IrExpr::AbsentArgument`] of type `ty`.
    pub(super) fn absent_argument(&mut self, ty: Ty) -> Result<(), Unsupported> {
        let carrier =
            carrier(ty)?.ok_or_else(|| "a defaulted call with a `Unit` parameter".to_string())?;
        push_zero(&mut self.code, carrier);
        Ok(())
    }
}

pub(super) fn push_zero(code: &mut Code, carrier: ValType) {
    match carrier {
        ValType::I32 => code.i32_const(0),
        ValType::I64 => code.i64_const(0),
        ValType::F32 => code.f32_const(0.0),
        ValType::F64 => code.f64_const(0.0),
        ValType::Ref { .. } => code.ref_null(HeapType::None),
    };
}
