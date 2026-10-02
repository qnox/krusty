//! Integer-constant magnitude and the one range test shared by checking and signature evaluation.
//!
//! An unsigned literal is a `UInt` magnitude. Values above `i32::MAX` are still in that range
//! (`2147483648u`, `UInt.MAX_VALUE`) and must keep every bit: truncating them to `i32` drops the
//! constant and a sibling `ULong` can no longer adopt them.

use crate::types::Ty;

/// A folded integer constant. Signed values are `Int` (`i32`). Unsigned values are the full `UInt`
/// magnitude in `0..=u32::MAX`, carried in `u64` so a value above `i32::MAX` is not truncated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegerConstant {
    Signed(i32),
    Unsigned(u64),
    /// Signed `Int` arithmetic that reached a division or remainder by zero.
    ///
    /// There is no magnitude. Every `Int` still adapts to `Long`, and this does not narrow.
    /// Executing the expression throws; a `const val` cannot publish it.
    DivisionByZero,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IntegerConstantOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
}

impl IntegerConstant {
    pub(crate) fn is_unsigned(self) -> bool {
        matches!(self, Self::Unsigned(_))
    }

    /// Expected primitive of a division-by-zero `Int` constant.
    ///
    /// Every `Int` adapts to `Long`. Without a magnitude, `Byte` and `Short` stay `Int`.
    pub(crate) fn division_by_zero_type(expected: Option<Ty>) -> Ty {
        expected
            .map(Ty::non_null)
            .map(|expected| match expected {
                Ty::TyParam(_, bound) => bound.non_null(),
                expected => expected,
            })
            .filter(|ty| *ty == Ty::Long)
            .unwrap_or(Ty::Int)
    }

    /// Whether this magnitude is a valid constant of `target`.
    ///
    /// This is the only range test. Representative selection and both adaptation paths call it, so
    /// the checker and the signature evaluator cannot disagree about what fits.
    pub(crate) fn fits(self, target: Ty) -> bool {
        match self {
            Self::Signed(value) => match target {
                Ty::Byte => i8::try_from(value).is_ok(),
                Ty::Short => i16::try_from(value).is_ok(),
                Ty::Int => true,
                Ty::Long => true,
                _ => false,
            },
            // No magnitude is available, so a narrower integer cannot be proven to fit.
            Self::DivisionByZero => matches!(target, Ty::Int | Ty::Long),
            Self::Unsigned(value) => match target {
                Ty::UByte => u8::try_from(value).is_ok(),
                Ty::UShort => u16::try_from(value).is_ok(),
                Ty::UInt => u32::try_from(value).is_ok(),
                Ty::ULong => true,
                _ => false,
            },
        }
    }

    /// Fold an operation in the constant's ordinary source width. Kotlin integral arithmetic wraps
    /// before a contextual conversion, including in a compile-time constant expression. Signed
    /// division or remainder by zero has no magnitude ([`IntegerConstant::DivisionByZero`]): the
    /// expression is still an `Int` constant and throws when executed. Signed minimum divided by
    /// `-1` keeps the wrapped minimum value and its remainder is zero, as on the JVM.
    pub(crate) fn fold(self, operation: IntegerConstantOp, right: Self) -> Option<Self> {
        match (self, right) {
            (Self::DivisionByZero, Self::Signed(_) | Self::DivisionByZero)
            | (Self::Signed(_), Self::DivisionByZero) => Some(Self::DivisionByZero),
            (Self::Signed(left), Self::Signed(right)) => {
                if matches!(
                    operation,
                    IntegerConstantOp::Divide | IntegerConstantOp::Remainder
                ) && right == 0
                {
                    return Some(Self::DivisionByZero);
                }
                let value = match operation {
                    IntegerConstantOp::Add => left.wrapping_add(right),
                    IntegerConstantOp::Subtract => left.wrapping_sub(right),
                    IntegerConstantOp::Multiply => left.wrapping_mul(right),
                    IntegerConstantOp::Divide => left
                        .checked_div(right)
                        .or_else(|| (left == i32::MIN && right == -1).then_some(i32::MIN))?,
                    IntegerConstantOp::Remainder => left
                        .checked_rem(right)
                        .or_else(|| (left == i32::MIN && right == -1).then_some(0))?,
                };
                Some(Self::Signed(value))
            }
            (Self::Unsigned(left), Self::Unsigned(right)) => {
                let left = u32::try_from(left).ok()?;
                let right = u32::try_from(right).ok()?;
                let value = match operation {
                    IntegerConstantOp::Add => left.wrapping_add(right),
                    IntegerConstantOp::Subtract => left.wrapping_sub(right),
                    IntegerConstantOp::Multiply => left.wrapping_mul(right),
                    IntegerConstantOp::Divide => left.checked_div(right)?,
                    IntegerConstantOp::Remainder => left.checked_rem(right)?,
                };
                Some(Self::Unsigned(u64::from(value)))
            }
            _ => None,
        }
    }

    /// One constituent that fails a narrower integer type whenever any constituent does.
    ///
    /// `200` does not fit in `Byte` and `-129` does not fit in `Byte`, so the representative
    /// refuses that target. Mixed signedness is not one constant. An empty list is not one either.
    pub(crate) fn representative(values: &[Self]) -> Option<Self> {
        let first = *values.first()?;
        if values
            .iter()
            .any(|value| value.is_unsigned() != first.is_unsigned())
        {
            return None;
        }
        let ladder: &[Ty] = if first.is_unsigned() {
            &[Ty::UByte, Ty::UShort, Ty::UInt]
        } else {
            &[Ty::Byte, Ty::Short, Ty::Int]
        };
        values.iter().copied().max_by_key(|constant| {
            ladder
                .iter()
                .filter(|target| !constant.fits(**target))
                .count()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_preserves_width_and_wraps_overflow() {
        assert_eq!(
            IntegerConstant::Signed(2)
                .fold(IntegerConstantOp::Multiply, IntegerConstant::Signed(3),),
            Some(IntegerConstant::Signed(6))
        );
        assert_eq!(
            IntegerConstant::Unsigned(65_535)
                .fold(IntegerConstantOp::Add, IntegerConstant::Unsigned(1),),
            Some(IntegerConstant::Unsigned(65_536))
        );
        assert_eq!(
            IntegerConstant::Unsigned(u64::from(u32::MAX))
                .fold(IntegerConstantOp::Add, IntegerConstant::Unsigned(1),),
            Some(IntegerConstant::Unsigned(0))
        );
        assert_eq!(
            IntegerConstant::Signed(i32::MIN)
                .fold(IntegerConstantOp::Divide, IntegerConstant::Signed(-1),),
            Some(IntegerConstant::Signed(i32::MIN))
        );
        assert_eq!(
            IntegerConstant::Signed(1).fold(IntegerConstantOp::Add, IntegerConstant::Unsigned(1),),
            None
        );
        assert_eq!(
            IntegerConstant::Signed(1).fold(IntegerConstantOp::Divide, IntegerConstant::Signed(0),),
            Some(IntegerConstant::DivisionByZero)
        );
        assert_eq!(
            IntegerConstant::Signed(1)
                .fold(IntegerConstantOp::Remainder, IntegerConstant::Signed(0),),
            Some(IntegerConstant::DivisionByZero)
        );
        assert_eq!(
            IntegerConstant::DivisionByZero
                .fold(IntegerConstantOp::Add, IntegerConstant::Signed(1),),
            Some(IntegerConstant::DivisionByZero)
        );
        assert_eq!(
            IntegerConstant::Unsigned(1)
                .fold(IntegerConstantOp::Divide, IntegerConstant::Unsigned(0),),
            None
        );
        assert!(!IntegerConstant::DivisionByZero.fits(Ty::Byte));
        assert!(!IntegerConstant::DivisionByZero.fits(Ty::Short));
        assert!(IntegerConstant::DivisionByZero.fits(Ty::Int));
        assert!(IntegerConstant::DivisionByZero.fits(Ty::Long));
        let joined = IntegerConstant::representative(&[
            IntegerConstant::DivisionByZero,
            IntegerConstant::Signed(1),
        ])
        .expect("division by zero shares the signed family");
        assert!(!joined.fits(Ty::Byte));
        assert!(joined.fits(Ty::Long));
    }

    #[test]
    fn representative_uses_the_shared_range_and_keeps_a_wide_unsigned_magnitude() {
        let signed = IntegerConstant::representative(&[
            IntegerConstant::Signed(-129),
            IntegerConstant::Signed(127),
        ])
        .expect("signed representative");
        assert!(!signed.fits(Ty::Byte));
        assert!(signed.fits(Ty::Short));
        assert!(signed.fits(Ty::Long));

        let above_i32 = IntegerConstant::Unsigned(u64::from(i32::MAX as u32) + 1);
        assert!(above_i32.fits(Ty::UInt));
        assert!(above_i32.fits(Ty::ULong));
        assert!(!above_i32.fits(Ty::UShort));

        let at_uint_max = IntegerConstant::Unsigned(u64::from(u32::MAX));
        assert!(at_uint_max.fits(Ty::UInt));
        assert!(at_uint_max.fits(Ty::ULong));
        assert!(!at_uint_max.fits(Ty::UByte));

        let joined = IntegerConstant::representative(&[above_i32, IntegerConstant::Unsigned(0)])
            .expect("unsigned representative");
        assert!(!joined.fits(Ty::UShort));
        assert!(joined.fits(Ty::ULong));

        assert_eq!(
            IntegerConstant::representative(&[
                IntegerConstant::Signed(1),
                IntegerConstant::Unsigned(1),
            ]),
            None
        );
    }
}
