//! Kotlin's conversions between numeric values (`Int.toLong()`, `Double.toInt()`, `Long.toByte()`),
//! on the carriers Wasm holds them in: a `Byte`, `Short`, `Char` or `Int` in an `i32`, which keeps a
//! narrow value's own bits (sign-extended, or zero-extended for a `Char`), a `Long` in an `i64`, a
//! `Float` in an `f32` and a `Double` in an `f64`.

use krusty::types::Ty;

use super::encode::{Code, ValType};
use super::runtime::{AND32, EXTEND_I32_S};

/// The carrier of a numeric or character value; `None` for any other type.
pub(super) fn numeric_carrier(ty: Ty) -> Option<ValType> {
    match ty {
        Ty::Byte | Ty::Short | Ty::Char | Ty::Int => Some(ValType::I32),
        Ty::Long => Some(ValType::I64),
        Ty::Float => Some(ValType::F32),
        Ty::Double => Some(ValType::F64),
        _ => None,
    }
}

/// Replace the `from` value on the stack with Kotlin's conversion of it to `to`: an integral value
/// keeps its low bits, a floating value rounds towards zero saturating at the result's bounds with
/// NaN as zero, a wider carrier takes the nearest value it holds, and a `Double` becomes the
/// nearest `Float`.
pub(super) fn convert(code: &mut Code, from: Ty, to: Ty) -> Result<(), String> {
    let (Some(source), Some(target)) = (numeric_carrier(from), numeric_carrier(to)) else {
        return Err(format!("a numeric conversion from `{from:?}` to `{to:?}`"));
    };
    if !widen(code, source, target) {
        narrow(code, source, target);
    }
    fit(code, from, to);
    Ok(())
}

/// Convert the value on the stack from the carrier `from` to the wider `to`; whether `to` is wider.
pub(super) fn widen(code: &mut Code, from: ValType, to: ValType) -> bool {
    let opcode = match (from, to) {
        (ValType::I32, ValType::I64) => EXTEND_I32_S,
        (ValType::I32, ValType::F32) => 0xb2,
        (ValType::I64, ValType::F32) => 0xb4,
        (ValType::I32, ValType::F64) => 0xb7,
        (ValType::I64, ValType::F64) => 0xb9,
        (ValType::F32, ValType::F64) => 0xbb,
        _ => return false,
    };
    code.op(opcode);
    true
}

/// Convert the value on the stack from the carrier `from` to the narrower or equal `to`.
fn narrow(code: &mut Code, from: ValType, to: ValType) {
    match (from, to) {
        (ValType::I64, ValType::I32) => {
            code.op(0xa7);
        }
        (ValType::F32, ValType::I32) => {
            code.trunc_sat(0);
        }
        (ValType::F64, ValType::I32) => {
            code.trunc_sat(2);
        }
        (ValType::F32, ValType::I64) => {
            code.trunc_sat(4);
        }
        (ValType::F64, ValType::I64) => {
            code.trunc_sat(6);
        }
        (ValType::F64, ValType::F32) => {
            code.op(0xb6);
        }
        _ => {}
    }
}

/// Wrap the `i32` on the stack, a `from` value, to the width of `to` when that is a narrow
/// integer: the carrier must hold the narrow value's own bits. `UByte` and `UShort` keep their
/// low bits even when `from` is already that type (`255u + 1` stored in a `UByte` is `0`).
pub(super) fn fit(code: &mut Code, from: Ty, to: Ty) {
    match to {
        Ty::UByte => {
            code.i32_const(0xff).op(AND32);
        }
        Ty::UShort => {
            code.i32_const(0xffff).op(AND32);
        }
        _ if from == to => {}
        Ty::Byte => {
            code.op(0xc0);
        }
        Ty::Short => {
            code.op(0xc1);
        }
        Ty::Char => {
            code.i32_const(0xffff).op(AND32);
        }
        _ => {}
    }
}
