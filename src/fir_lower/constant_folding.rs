//! Mechanical constant operations over already-checked FIR/common IR values.
//!
//! This module performs no lookup or type inference. It only realizes operations whose semantic
//! identity and operand types were fixed by the body checker.

use crate::ir::{IrConst, IrExpr};

pub(super) fn negate(constant: &IrConst) -> Option<IrConst> {
    Some(match constant {
        IrConst::Byte(value) => IrConst::Byte(value.checked_neg()?),
        IrConst::Short(value) => IrConst::Short(value.checked_neg()?),
        IrConst::Int(value) => IrConst::Int(value.checked_neg()?),
        IrConst::Long(value) => IrConst::Long(value.checked_neg()?),
        IrConst::Float(value) => IrConst::Float(-value),
        IrConst::Double(value) => IrConst::Double(-value),
        IrConst::Boolean(_) | IrConst::Char(_) | IrConst::String(_) | IrConst::Null => return None,
    })
}

/// Kotlin metadata's `HAS_CONSTANT` fact for an ordinary immutable property's checked initializer.
/// This does not make the property a source-level `const val`; it only describes the initializer
/// encoded in metadata.
pub(super) fn is_metadata_constant(expression: &IrExpr) -> bool {
    matches!(
        expression,
        IrExpr::Const(
            IrConst::Boolean(_)
                | IrConst::Byte(_)
                | IrConst::Short(_)
                | IrConst::Int(_)
                | IrConst::Long(_)
                | IrConst::Float(_)
                | IrConst::Double(_)
                | IrConst::Char(_)
                | IrConst::String(_)
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_numeric_negation_folds_without_reinterpreting_the_source() {
        assert_eq!(negate(&IrConst::Int(9)), Some(IrConst::Int(-9)));
        assert_eq!(negate(&IrConst::Boolean(true)), None);
    }

    #[test]
    fn null_is_not_a_metadata_constant_initializer() {
        assert!(is_metadata_constant(&IrExpr::Const(IrConst::Int(0))));
        assert!(!is_metadata_constant(&IrExpr::Const(IrConst::Null)));
    }
}
