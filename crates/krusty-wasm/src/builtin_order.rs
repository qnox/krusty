//! The order of two values of one builtin scalar class, as its `compareTo` answers it.
//!
//! An integral class (and `Boolean` and `Char`) orders by value, answering `-1`, `0` or `1`, as
//! a direct call of the source operation lowers. A floating class orders totally: `-0.0` is below
//! `0.0`, and every `NaN` is equal to every other and above every number, which is the order of
//! the values' bits once each `NaN` is the canonical one.

use krusty::types::Ty;

use super::encode::{BlockType, Code, ValType};
use super::numeric_conversions;
use super::runtime::{GT_S32, GT_S64, LT_S32, LT_S64, SUB32};

const SELECT: u8 = 0x1b;

/// The carrier of a builtin scalar class whose `compareTo` this module writes.
pub(super) fn ordered_carrier(ty: Ty) -> Option<ValType> {
    match ty {
        Ty::Boolean => Some(ValType::I32),
        _ => numeric_conversions::numeric_carrier(ty),
    }
}

/// The scratch locals [`emit_order`] needs beyond the two operands', in order.
pub(super) fn order_scratch(carrier: ValType) -> Vec<ValType> {
    match carrier {
        ValType::F32 => vec![ValType::I32, ValType::I32],
        ValType::F64 => vec![ValType::I64, ValType::I64],
        _ => Vec::new(),
    }
}

/// Push `left.compareTo(right)` for two locals of `carrier`; `scratch` is the first of the
/// locals [`order_scratch`] asked for.
pub(super) fn emit_order(code: &mut Code, carrier: ValType, left: u32, right: u32, scratch: u32) {
    let signed = |code: &mut Code, left: u32, right: u32, (greater, less): (u8, u8)| {
        code.local_get(left)
            .local_get(right)
            .op(greater)
            .local_get(left)
            .local_get(right)
            .op(less)
            .op(SUB32);
    };
    let (less, greater, not_equal, reinterpret, bits) = match carrier {
        ValType::I32 => return signed(code, left, right, (GT_S32, LT_S32)),
        ValType::I64 => return signed(code, left, right, (GT_S64, LT_S64)),
        ValType::F32 => (0x5d, 0x5e, 0x5c, 0xbc, (GT_S32, LT_S32)),
        ValType::F64 => (0x63, 0x64, 0x62, 0xbd, (GT_S64, LT_S64)),
        _ => unreachable!("`ordered_carrier` admits only scalar carriers"),
    };
    let (left_bits, right_bits) = (scratch, scratch + 1);
    for (value, target) in [(left, left_bits), (right, right_bits)] {
        if carrier == ValType::F32 {
            code.i32_const(0x7fc0_0000);
        } else {
            code.i64_const(0x7ff8_0000_0000_0000);
        }
        code.local_get(value)
            .op(reinterpret)
            .local_get(value)
            .local_get(value)
            .op(not_equal)
            .op(SELECT)
            .local_set(target);
    }
    code.local_get(left)
        .local_get(right)
        .op(less)
        .if_(BlockType::Value(ValType::I32))
        .i32_const(-1)
        .else_()
        .local_get(left)
        .local_get(right)
        .op(greater)
        .if_(BlockType::Value(ValType::I32))
        .i32_const(1)
        .else_();
    signed(code, left_bits, right_bits, bits);
    code.end().end();
}
