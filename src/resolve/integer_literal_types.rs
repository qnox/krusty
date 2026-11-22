//! The contextual type of an integer literal: Kotlin types a literal from its expected type when
//! the value fits, and defaults it to `Int` or `UInt` otherwise.

use crate::ast::UnOp;
use crate::integer_constant::IntegerConstant;
use crate::types::{Ty, TyKind};

/// The type of an unsigned integer literal (`200u`). Kotlin types an integer literal from its
/// CONTEXT, unsigned exactly like signed: `val a: UByte = 200u` is a `UByte` the same way
/// `val b: Byte = 100` is a `Byte`, and `val c: ULong = 7u` is a `ULong`. Absent an unsigned
/// expected type the literal is `UInt` (the parser already promoted anything above `UInt.MAX` to a
/// `ULongLit`). A magnitude that does NOT fit the expected type stays `UInt` so the ordinary
/// initializer-mismatch diagnostic reports it instead of the value silently truncating.
pub(super) fn unsigned_literal_ty(value: i64, expected: Option<Ty>) -> Ty {
    let Some(constant) = u64::try_from(value).ok().map(IntegerConstant::Unsigned) else {
        return Ty::UInt;
    };
    expected
        .map(Ty::non_null)
        .map(|expected| match expected.kind() {
            TyKind::TyParam(_, bound) | TyKind::Var(_, bound) => bound.non_null(),
            _ => expected,
        })
        .filter(|ty| constant.fits(*ty))
        .unwrap_or(Ty::UInt)
}

/// Kotlin signed integer literals take a concrete built-in type from their expected context when the
/// value fits; without one they default to `Int`. Overload selection already uses this literal rule,
/// so checking and selected-call materialization must use the same type instead of reverting to `Int`.
pub(super) fn signed_literal_ty(value: i64, expected: Option<Ty>) -> Ty {
    let fits = |ty: Ty| match ty {
        Ty::Long => true,
        other => i32::try_from(value).is_ok_and(|value| IntegerConstant::Signed(value).fits(other)),
    };
    expected
        .map(Ty::non_null)
        .map(|expected| match expected.kind() {
            TyKind::TyParam(_, bound) | TyKind::Var(_, bound) => bound.non_null(),
            _ => expected,
        })
        .filter(|ty| fits(*ty))
        .unwrap_or(Ty::Int)
}

/// Contextual type of a signed integer constant written with a leading `+` or `-`. The parser keeps
/// the sign as a unary expression, but Kotlin treats the complete constant as the literal for
/// expected-type narrowing (`val b: Byte? = -1`). Apply the range check to the signed value so the
/// minimum value of each primitive remains representable.
pub(super) fn signed_unary_literal_ty(
    value: i64,
    operator: UnOp,
    expected: Option<Ty>,
) -> Option<Ty> {
    let value = match operator {
        UnOp::Neg => -(value as i128),
        UnOp::Plus => value as i128,
        UnOp::Not => return None,
    };
    let expected = expected?.non_null();
    let fits = match expected {
        Ty::Byte => (i8::MIN as i128..=i8::MAX as i128).contains(&value),
        Ty::Short => (i16::MIN as i128..=i16::MAX as i128).contains(&value),
        Ty::Int => (i32::MIN as i128..=i32::MAX as i128).contains(&value),
        Ty::Long => true,
        _ => false,
    };
    fits.then_some(expected)
}
