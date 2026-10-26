//! Constant evaluation of the stdlib numeric declarations the provider classifies as
//! [`ConstNumericOperation`]s, and of the unsigned bitwise intrinsics.
//!
//! Every value is read as the exact integer it denotes (a signed type sign-extended, an unsigned
//! type zero-extended), computed exactly, and wrapped to the declared result type, which is how
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
            let value = match operation {
                ConstNumericOperation::Add => lhs + rhs,
                ConstNumericOperation::Subtract => lhs - rhs,
                ConstNumericOperation::Multiply => lhs * rhs,
                ConstNumericOperation::Divide => lhs.checked_div(rhs)?,
                ConstNumericOperation::Remainder => lhs.checked_rem(rhs)?,
                ConstNumericOperation::FloorDivide => {
                    let quotient = lhs.checked_div(rhs)?;
                    if lhs % rhs != 0 && (lhs < 0) != (rhs < 0) {
                        quotient - 1
                    } else {
                        quotient
                    }
                }
                ConstNumericOperation::Modulo => {
                    let remainder = lhs.checked_rem(rhs)?;
                    if remainder != 0 && ((remainder < 0) != (rhs < 0)) {
                        remainder + rhs
                    } else {
                        remainder
                    }
                }
                ConstNumericOperation::Conversion | ConstNumericOperation::CompareTo => {
                    return None
                }
            };
            integral_constant(value, result)
        }
        _ => None,
    }
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
        FirBinaryOperation::ShiftLeft => lhs << shift_distance(rhs, width)?,
        FirBinaryOperation::ShiftRight | FirBinaryOperation::UnsignedShiftRight => {
            lhs >> shift_distance(rhs, width)?
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
}
