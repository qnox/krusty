//! Scalar machine instructions a target stdlib declares as its own implementation.
//!
//! A target stdlib spells some declarations as a single instruction over fixed-width scalars
//! (Kotlin/Wasm's `@WasmOp`). The provider authenticates the declaration and publishes the
//! instruction together with the declaration's exact operand and result types; the instruction
//! has the target instruction's semantics on those operands (an integral division by zero, for
//! one, is the target's trap, not Kotlin's exception: the stdlib guards each such call itself).

use crate::types::Ty;

/// The operation of one scalar instruction. Operand and result widths come from the authenticated
/// declaration's semantic types, never from this identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScalarInstruction {
    CountLeadingZeros,
    CountTrailingZeros,
    PopulationCount,
    Absolute,
    Negate,
    Ceiling,
    Floor,
    /// Rounds towards zero, keeping the floating type.
    Truncate,
    /// Rounds to the nearest integral value, ties to even, keeping the floating type.
    Nearest,
    SquareRoot,
    Add,
    Subtract,
    Multiply,
    /// Signed integral or IEEE floating division.
    Divide,
    DivideUnsigned,
    /// Signed integral remainder.
    Remainder,
    RemainderUnsigned,
    And,
    Or,
    Xor,
    ShiftLeft,
    ShiftRight,
    ShiftRightUnsigned,
    RotateLeft,
    RotateRight,
    /// IEEE minimum and maximum: a NaN operand yields NaN, and `-0.0` orders below `0.0`.
    Minimum,
    Maximum,
    CopySign,
    Compare(ScalarComparison),
    /// Keeps the low 32 bits of a 64-bit integral operand.
    Wrap,
    Extend,
    ExtendUnsigned,
    ConvertToFloating,
    ConvertUnsignedToFloating,
    /// Floating to integral, rounding towards zero and saturating at the result's bounds; NaN is 0.
    TruncateSaturating,
    TruncateSaturatingUnsigned,
    Promote,
    Demote,
}

/// A comparison producing `Boolean`. Ordered comparisons of floating operands are IEEE (false
/// when either operand is NaN); the unsigned forms apply only to integral operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScalarComparison {
    Equal,
    NotEqual,
    Less,
    LessUnsigned,
    Greater,
    GreaterUnsigned,
    LessOrEqual,
    LessOrEqualUnsigned,
    GreaterOrEqual,
    GreaterOrEqualUnsigned,
}

/// The physical class of a scalar instruction operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarClass {
    Integral32,
    Integral64,
    Float32,
    Float64,
}

impl ScalarClass {
    /// The class of a semantic scalar type. An unsigned type shares its signed carrier's class:
    /// the instruction, not the type, selects signedness.
    pub fn of(ty: Ty) -> Option<Self> {
        Some(match ty {
            Ty::Int | Ty::UInt => Self::Integral32,
            Ty::Long | Ty::ULong => Self::Integral64,
            Ty::Float => Self::Float32,
            Ty::Double => Self::Float64,
            _ => return None,
        })
    }

    pub const fn is_integral(self) -> bool {
        matches!(self, Self::Integral32 | Self::Integral64)
    }
}

impl ScalarInstruction {
    /// How many operands the instruction consumes.
    pub const fn arity(self) -> usize {
        match self {
            Self::Add
            | Self::Subtract
            | Self::Multiply
            | Self::Divide
            | Self::DivideUnsigned
            | Self::Remainder
            | Self::RemainderUnsigned
            | Self::And
            | Self::Or
            | Self::Xor
            | Self::ShiftLeft
            | Self::ShiftRight
            | Self::ShiftRightUnsigned
            | Self::RotateLeft
            | Self::RotateRight
            | Self::Minimum
            | Self::Maximum
            | Self::CopySign
            | Self::Compare(_) => 2,
            _ => 1,
        }
    }

    /// Whether the instruction is defined with every operand of type `operand` and a `result`.
    /// This is the whole typing rule: authentication, IR validation, and target emission share it.
    pub fn admits(self, operand: Ty, result: Ty) -> bool {
        let Some(from) = ScalarClass::of(operand) else {
            return false;
        };
        use ScalarClass::{Float32, Float64, Integral32, Integral64};
        match self {
            Self::Add | Self::Subtract | Self::Multiply | Self::Divide => result == operand,
            Self::CountLeadingZeros
            | Self::CountTrailingZeros
            | Self::PopulationCount
            | Self::DivideUnsigned
            | Self::Remainder
            | Self::RemainderUnsigned
            | Self::And
            | Self::Or
            | Self::Xor
            | Self::ShiftLeft
            | Self::ShiftRight
            | Self::ShiftRightUnsigned
            | Self::RotateLeft
            | Self::RotateRight => from.is_integral() && result == operand,
            Self::Absolute
            | Self::Negate
            | Self::Ceiling
            | Self::Floor
            | Self::Truncate
            | Self::Nearest
            | Self::SquareRoot
            | Self::Minimum
            | Self::Maximum
            | Self::CopySign => !from.is_integral() && result == operand,
            Self::Compare(comparison) => {
                result == Ty::Boolean && (from.is_integral() || !comparison.is_unsigned())
            }
            Self::Wrap => from == Integral64 && ScalarClass::of(result) == Some(Integral32),
            Self::Extend | Self::ExtendUnsigned => {
                from == Integral32 && ScalarClass::of(result) == Some(Integral64)
            }
            Self::ConvertToFloating | Self::ConvertUnsignedToFloating => {
                from.is_integral() && matches!(ScalarClass::of(result), Some(Float32 | Float64))
            }
            Self::TruncateSaturating | Self::TruncateSaturatingUnsigned => {
                !from.is_integral() && ScalarClass::of(result).is_some_and(ScalarClass::is_integral)
            }
            Self::Promote => from == Float32 && ScalarClass::of(result) == Some(Float64),
            Self::Demote => from == Float64 && ScalarClass::of(result) == Some(Float32),
        }
    }
}

impl ScalarComparison {
    pub const fn is_unsigned(self) -> bool {
        matches!(
            self,
            Self::LessUnsigned
                | Self::GreaterUnsigned
                | Self::LessOrEqualUnsigned
                | Self::GreaterOrEqualUnsigned
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ScalarComparison, ScalarInstruction};
    use crate::types::Ty;

    #[test]
    fn typing_rule_separates_integral_floating_and_width_changing_instructions() {
        assert!(ScalarInstruction::CountLeadingZeros.admits(Ty::Long, Ty::Long));
        assert!(!ScalarInstruction::CountLeadingZeros.admits(Ty::Double, Ty::Double));
        assert!(!ScalarInstruction::CountLeadingZeros.admits(Ty::Long, Ty::Int));
        assert!(ScalarInstruction::Divide.admits(Ty::Double, Ty::Double));
        assert!(ScalarInstruction::DivideUnsigned.admits(Ty::UInt, Ty::UInt));
        assert!(!ScalarInstruction::DivideUnsigned.admits(Ty::Float, Ty::Float));
        assert!(ScalarInstruction::SquareRoot.admits(Ty::Float, Ty::Float));
        assert!(!ScalarInstruction::SquareRoot.admits(Ty::Int, Ty::Int));
        assert!(ScalarInstruction::Compare(ScalarComparison::Less).admits(Ty::Double, Ty::Boolean));
        assert!(!ScalarInstruction::Compare(ScalarComparison::LessUnsigned)
            .admits(Ty::Double, Ty::Boolean));
        assert!(ScalarInstruction::Wrap.admits(Ty::Long, Ty::Int));
        assert!(!ScalarInstruction::Wrap.admits(Ty::Int, Ty::Long));
        assert!(ScalarInstruction::ExtendUnsigned.admits(Ty::Int, Ty::ULong));
        assert!(ScalarInstruction::TruncateSaturatingUnsigned.admits(Ty::Double, Ty::UInt));
        assert!(ScalarInstruction::ConvertUnsignedToFloating.admits(Ty::Long, Ty::Float));
        assert!(ScalarInstruction::Promote.admits(Ty::Float, Ty::Double));
        assert!(!ScalarInstruction::Demote.admits(Ty::Float, Ty::Double));
        assert!(!ScalarInstruction::Add.admits(Ty::Boolean, Ty::Boolean));
        assert_eq!(ScalarInstruction::CopySign.arity(), 2);
        assert_eq!(ScalarInstruction::Nearest.arity(), 1);
    }
}
