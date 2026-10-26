//! Constant evaluation of the stdlib numeric declarations the provider classifies as
//! [`ConstNumericOperation`]s, and of the unsigned bitwise intrinsics.
//!
//! Every value is read as the exact integer it denotes (a signed type sign-extended, an unsigned
//! type zero-extended) in an `i128` host, computed with wrapping `i128` arithmetic, and wrapped
//! to the declared result type. Operands and results are at most 64 bits wide, so a quotient or
//! remainder is exact in the host and a sum, difference or product keeps the low 64 bits the
//! result needs: `ULong.MAX_VALUE * ULong.MAX_VALUE` is `1uL` and `Long.MIN_VALUE.floorDiv(-1L)`
//! is `Long.MIN_VALUE`, without overflowing the host. This is how
//! the stdlib bodies behave: an unsigned operation on two widths widens both to the wider type
//! (at least `UInt`), and a signed `floorDiv`/`mod` computes at the wider of `Int` and its operands
//! before converting to its declared result.

use crate::fir::{FirBinaryOperation, FirConstant};
use crate::types::{ConstNumericOperation, Ty};

use super::{unsigned, EvaluatedConstant};

/// The value of `operation` on a constant receiver and arguments, as the declaration's `result`.
/// A division by zero throws at run time, which kotlinc's interpreter leaves to the call.
pub(super) fn evaluate(
    operation: ConstNumericOperation,
    receiver: &EvaluatedConstant,
    arguments: &[EvaluatedConstant],
    result: Ty,
) -> Option<EvaluatedConstant> {
    match (operation, arguments) {
        (ConstNumericOperation::Conversion, []) => convert(receiver, result),
        (ConstNumericOperation::CompareTo, [other]) => Some(EvaluatedConstant {
            value: FirConstant::Int(unsigned(receiver)?.cmp(&unsigned(other)?) as i64),
            ty: Ty::Int,
        }),
        (_, [other]) if is_floating(receiver.ty) => {
            floating_modulo(operation, receiver, other, result)
        }
        (_, [other]) => {
            let (lhs, rhs) = (integral(receiver)?, integral(other)?);
            let value = wrapping_integral(operation, lhs, rhs)?;
            integral_constant(value, result)
        }
        _ => None,
    }
}

/// `operation` on two exact integral operands with wrapping `i128` arithmetic; the caller wraps
/// the value to the declared result type, whose bits are the low bits of this value. A zero
/// divisor is `None`: the division throws at run time.
fn wrapping_integral(operation: ConstNumericOperation, lhs: i128, rhs: i128) -> Option<i128> {
    if rhs == 0
        && matches!(
            operation,
            ConstNumericOperation::Divide
                | ConstNumericOperation::Remainder
                | ConstNumericOperation::FloorDivide
                | ConstNumericOperation::Modulo
        )
    {
        return None;
    }
    Some(match operation {
        ConstNumericOperation::Add => lhs.wrapping_add(rhs),
        ConstNumericOperation::Subtract => lhs.wrapping_sub(rhs),
        ConstNumericOperation::Multiply => lhs.wrapping_mul(rhs),
        ConstNumericOperation::Divide => lhs.wrapping_div(rhs),
        ConstNumericOperation::Remainder => lhs.wrapping_rem(rhs),
        ConstNumericOperation::FloorDivide => {
            let quotient = lhs.wrapping_div(rhs);
            if lhs.wrapping_rem(rhs) != 0 && (lhs < 0) != (rhs < 0) {
                quotient.wrapping_sub(1)
            } else {
                quotient
            }
        }
        ConstNumericOperation::Modulo => {
            let remainder = lhs.wrapping_rem(rhs);
            if remainder != 0 && ((remainder < 0) != (rhs < 0)) {
                remainder.wrapping_add(rhs)
            } else {
                remainder
            }
        }
        ConstNumericOperation::Conversion | ConstNumericOperation::CompareTo => return None,
    })
}

/// An unsigned `and`/`or`/`xor`/`shl`/`shr` at `ty`'s width; a shift distance is masked to it.
pub(super) fn unsigned_bitwise(
    operation: FirBinaryOperation,
    lhs: &EvaluatedConstant,
    rhs: &EvaluatedConstant,
    ty: Ty,
) -> Option<EvaluatedConstant> {
    let width = unsigned_width(ty)?;
    let lhs = i128::from(unsigned(lhs)?);
    let value = match operation {
        FirBinaryOperation::BitwiseAnd => lhs & i128::from(unsigned(rhs)?),
        FirBinaryOperation::BitwiseOr => lhs | i128::from(unsigned(rhs)?),
        FirBinaryOperation::BitwiseXor => lhs ^ i128::from(unsigned(rhs)?),
        FirBinaryOperation::ShiftLeft => lhs.wrapping_shl(shift_distance(rhs, width)?),
        FirBinaryOperation::ShiftRight | FirBinaryOperation::UnsignedShiftRight => {
            lhs.wrapping_shr(shift_distance(rhs, width)?)
        }
        _ => return None,
    };
    integral_constant(value, ty)
}

/// An unsigned `inv()`.
pub(super) fn unsigned_inverse(operand: &EvaluatedConstant, ty: Ty) -> Option<EvaluatedConstant> {
    unsigned_width(ty)?;
    integral_constant(!i128::from(unsigned(operand)?), ty)
}

fn shift_distance(distance: &EvaluatedConstant, width: u32) -> Option<u32> {
    let FirConstant::Int(distance) = distance.value else {
        return None;
    };
    Some((distance as u32) & (width - 1))
}

fn unsigned_width(ty: Ty) -> Option<u32> {
    Some(match ty {
        Ty::UInt => 32,
        Ty::ULong => 64,
        _ => return None,
    })
}

/// `Float`/`Double` `mod`: the remainder, moved to the divisor's sign when it is non-zero.
fn floating_modulo(
    operation: ConstNumericOperation,
    receiver: &EvaluatedConstant,
    other: &EvaluatedConstant,
    result: Ty,
) -> Option<EvaluatedConstant> {
    if operation != ConstNumericOperation::Modulo {
        return None;
    }
    let (lhs, rhs) = (floating(receiver)?, floating(other)?);
    let value = match result {
        Ty::Float => {
            let (lhs, rhs) = (lhs as f32, rhs as f32);
            let remainder = lhs % rhs;
            FirConstant::Float(if remainder != 0.0 && remainder.signum() != rhs.signum() {
                remainder + rhs
            } else {
                remainder
            })
        }
        Ty::Double => {
            let remainder = lhs % rhs;
            FirConstant::Double(if remainder != 0.0 && remainder.signum() != rhs.signum() {
                remainder + rhs
            } else {
                remainder
            })
        }
        _ => return None,
    };
    Some(EvaluatedConstant { value, ty: result })
}

/// A conversion to `result`: integral values wrap, an unsigned value converts to a floating type
/// through `Double` (as `UInt.toFloat()` and `ULong.toFloat()` do), and a floating value converts
/// to `UInt`/`ULong` truncating toward zero and saturating, NaN to zero.
fn convert(value: &EvaluatedConstant, result: Ty) -> Option<EvaluatedConstant> {
    if let Some(source) = floating(value) {
        let converted = match result {
            Ty::UInt => i128::from(source as u32),
            Ty::ULong => i128::from(source as u64),
            _ => return None,
        };
        return integral_constant(converted, result);
    }
    let source = integral(value)?;
    let converted = match result {
        Ty::Float => FirConstant::Float(source as f64 as f32),
        Ty::Double => FirConstant::Double(source as f64),
        _ => return integral_constant(source, result),
    };
    Some(EvaluatedConstant {
        value: converted,
        ty: result,
    })
}

/// The exact integer a signed or unsigned integral constant denotes.
fn integral(constant: &EvaluatedConstant) -> Option<i128> {
    match (&constant.value, constant.ty) {
        (FirConstant::Int(value), Ty::Byte | Ty::Short | Ty::Int) => Some(i128::from(*value)),
        (FirConstant::Long(value), Ty::Long) => Some(i128::from(*value)),
        _ => unsigned(constant).map(i128::from),
    }
}

fn floating(constant: &EvaluatedConstant) -> Option<f64> {
    match (&constant.value, constant.ty) {
        (FirConstant::Float(value), Ty::Float) => Some(f64::from(*value)),
        (FirConstant::Double(value), Ty::Double) => Some(*value),
        _ => None,
    }
}

fn is_floating(ty: Ty) -> bool {
    matches!(ty, Ty::Float | Ty::Double)
}

/// `value` wrapped to the integral type `ty`, in the representation checked FIR gives a literal
/// of that type.
fn integral_constant(value: i128, ty: Ty) -> Option<EvaluatedConstant> {
    let value = match ty {
        Ty::Byte => FirConstant::Int(i64::from(value as i8)),
        Ty::Short => FirConstant::Int(i64::from(value as i16)),
        Ty::Int => FirConstant::Int(i64::from(value as i32)),
        Ty::Long => FirConstant::Long(value as i64),
        Ty::UByte => FirConstant::UInt(i64::from(value as u8)),
        Ty::UShort => FirConstant::UInt(i64::from(value as u16)),
        Ty::UInt => FirConstant::UInt(i64::from(value as u32)),
        Ty::ULong => FirConstant::ULong(value as u64 as i64),
        _ => return None,
    };
    Some(EvaluatedConstant { value, ty })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(value: FirConstant, ty: Ty) -> EvaluatedConstant {
        EvaluatedConstant { value, ty }
    }

    #[test]
    fn floor_division_and_modulo_round_toward_negative_infinity() {
        let int = |value| constant(FirConstant::Int(value), Ty::Int);
        let apply = |operation, lhs, rhs| {
            evaluate(operation, &int(lhs), &[int(rhs)], Ty::Int).map(|value| value.value)
        };
        assert_eq!(
            apply(ConstNumericOperation::FloorDivide, -7, 2),
            Some(FirConstant::Int(-4))
        );
        assert_eq!(
            apply(ConstNumericOperation::FloorDivide, 7, 2),
            Some(FirConstant::Int(3))
        );
        assert_eq!(
            apply(ConstNumericOperation::Modulo, -7, 3),
            Some(FirConstant::Int(2))
        );
        assert_eq!(
            apply(ConstNumericOperation::Modulo, 7, -3),
            Some(FirConstant::Int(-2))
        );
        assert_eq!(apply(ConstNumericOperation::FloorDivide, 7, 0), None);
        assert_eq!(
            apply(ConstNumericOperation::FloorDivide, i64::from(i32::MIN), -1),
            Some(FirConstant::Int(i64::from(i32::MIN)))
        );
    }

    #[test]
    fn unsigned_operations_wrap_at_the_result_width() {
        let uint = |value| constant(FirConstant::UInt(value), Ty::UInt);
        assert_eq!(
            evaluate(
                ConstNumericOperation::Subtract,
                &uint(5),
                &[uint(7)],
                Ty::UInt
            )
            .map(|value| value.value),
            Some(FirConstant::UInt(4_294_967_294))
        );
        let ubyte = constant(FirConstant::UInt(7), Ty::UByte);
        assert_eq!(
            evaluate(
                ConstNumericOperation::Modulo,
                &uint(300),
                &[ubyte],
                Ty::UByte
            )
            .map(|value| value.value),
            Some(FirConstant::UInt(6))
        );
        let byte = constant(FirConstant::Int(-1), Ty::Byte);
        assert_eq!(
            evaluate(ConstNumericOperation::Conversion, &byte, &[], Ty::UInt)
                .map(|value| value.value),
            Some(FirConstant::UInt(4_294_967_295))
        );
        let double = constant(FirConstant::Double(-3.5), Ty::Double);
        assert_eq!(
            evaluate(ConstNumericOperation::Conversion, &double, &[], Ty::UInt)
                .map(|value| value.value),
            Some(FirConstant::UInt(0))
        );
    }

    #[test]
    fn integral_operations_wrap_at_the_result_width_without_overflowing_the_host() {
        let ulong = |value: u64| constant(FirConstant::ULong(value as i64), Ty::ULong);
        let long = |value| constant(FirConstant::Long(value), Ty::Long);
        let apply = |operation, lhs: &EvaluatedConstant, rhs: &EvaluatedConstant, result| {
            evaluate(operation, lhs, std::slice::from_ref(rhs), result).map(|value| value.value)
        };
        let max = ulong(u64::MAX);
        assert_eq!(
            apply(ConstNumericOperation::Multiply, &max, &max, Ty::ULong),
            Some(FirConstant::ULong(1))
        );
        assert_eq!(
            apply(ConstNumericOperation::Add, &max, &max, Ty::ULong),
            Some(FirConstant::ULong(u64::MAX.wrapping_add(u64::MAX) as i64))
        );
        assert_eq!(
            apply(ConstNumericOperation::Subtract, &ulong(0), &max, Ty::ULong),
            Some(FirConstant::ULong(1))
        );
        let uint_max = constant(FirConstant::UInt(i64::from(u32::MAX)), Ty::UInt);
        assert_eq!(
            apply(ConstNumericOperation::Multiply, &max, &uint_max, Ty::ULong),
            Some(FirConstant::ULong(
                u64::MAX.wrapping_mul(u64::from(u32::MAX)) as i64
            ))
        );
        assert_eq!(
            apply(ConstNumericOperation::Divide, &max, &max, Ty::ULong),
            Some(FirConstant::ULong(1))
        );
        assert_eq!(
            apply(
                ConstNumericOperation::Remainder,
                &max,
                &ulong(10),
                Ty::ULong
            ),
            Some(FirConstant::ULong(5))
        );
        assert_eq!(
            apply(ConstNumericOperation::Divide, &max, &ulong(0), Ty::ULong),
            None
        );
        assert_eq!(
            apply(ConstNumericOperation::Modulo, &max, &ulong(0), Ty::ULong),
            None
        );
        let (min, minus_one) = (long(i64::MIN), long(-1));
        assert_eq!(
            apply(
                ConstNumericOperation::FloorDivide,
                &min,
                &minus_one,
                Ty::Long
            ),
            Some(FirConstant::Long(i64::MIN))
        );
        assert_eq!(
            apply(ConstNumericOperation::Modulo, &min, &minus_one, Ty::Long),
            Some(FirConstant::Long(0))
        );
        assert_eq!(
            apply(ConstNumericOperation::Multiply, &min, &min, Ty::Long),
            Some(FirConstant::Long(0))
        );
        assert_eq!(
            unsigned_bitwise(
                FirBinaryOperation::ShiftLeft,
                &max,
                &constant(FirConstant::Int(63), Ty::Int),
                Ty::ULong
            )
            .map(|value| value.value),
            Some(FirConstant::ULong(i64::MIN))
        );
    }
}
