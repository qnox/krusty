//! The order of two values of one builtin scalar class, as its `compareTo` answers it: `-1`, `0`
//! or `1`.
//!
//! Each value becomes an integral order key, and the keys compare as signed integers. An
//! integral class (and `Boolean` and `Char`) is its own key. A floating value's key is its
//! canonical bits with the magnitude bits of a negative value flipped, which is the total order
//! `Float.compare`/`Double.compare` define: `-0.0` below `0.0`, and every `NaN` equal to every
//! other and above everything else.
//!
//! A direct call of the source operation and a builtin box's implementation of
//! `Comparable.compareTo` both order through here.

use krusty::types::Ty;

use super::encode::{Code, ValType};
use super::hashing::Hashing;
use super::numeric_conversions;
use super::runtime::{
    GT_S32, GT_S64, LT_S32, LT_S64, SHR_S32, SHR_S64, SHR_U32, SHR_U64, SUB32, XOR32, XOR64,
};

/// The order key's type of values of `ty`, when `ty` is a builtin scalar class.
pub(super) fn order_key(ty: Ty) -> Option<ValType> {
    match ty {
        Ty::Boolean => Some(ValType::I32),
        _ => match numeric_conversions::numeric_carrier(ty)? {
            ValType::I32 | ValType::F32 => Some(ValType::I32),
            ValType::I64 | ValType::F64 => Some(ValType::I64),
            _ => None,
        },
    }
}

/// Replace the `ty` value on the stack with its order key, and store it in `local`, a local of
/// [`order_key`]'s type.
pub(super) fn store_order_key(code: &mut Code, hashing: &Hashing, ty: Ty, local: u32) {
    if hashing.emit_float_bits(code, ty) {
        code.local_tee(local).local_get(local);
        match order_key(ty) {
            Some(ValType::I32) => code
                .i32_const(31)
                .op(SHR_S32)
                .i32_const(1)
                .op(SHR_U32)
                .op(XOR32),
            _ => code
                .i64_const(63)
                .op(SHR_S64)
                .i64_const(1)
                .op(SHR_U64)
                .op(XOR64),
        };
    }
    code.local_set(local);
}

/// Push the order of the keys in `left` and `right`, locals of type `key`.
pub(super) fn emit_key_order(code: &mut Code, key: ValType, left: u32, right: u32) {
    let (greater, less) = match key {
        ValType::I32 => (GT_S32, LT_S32),
        _ => (GT_S64, LT_S64),
    };
    code.local_get(left)
        .local_get(right)
        .op(greater)
        .local_get(left)
        .local_get(right)
        .op(less)
        .op(SUB32);
}
