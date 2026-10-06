//! Mechanical constant operations over already-checked FIR/common IR values.
//!
//! This module performs no lookup or type inference. It only realizes operations whose semantic
//! identity and operand types were fixed by the body checker.

use crate::ir::IrConst;

pub(super) fn negate(constant: &IrConst) -> Option<IrConst> {
    Some(match constant {
        IrConst::Byte(value) => IrConst::Byte(value.checked_neg()?),
        IrConst::Short(value) => IrConst::Short(value.checked_neg()?),
        IrConst::Int(value) => IrConst::Int(value.checked_neg()?),
        IrConst::Long(value) => IrConst::Long(value.checked_neg()?),
        // An unsigned constant has no negation to fold: Kotlin has no unary minus on the
        // unsigned types, so a `-200u` never reaches here.
        IrConst::UByte(_) | IrConst::UShort(_) | IrConst::UInt(_) | IrConst::ULong(_) => {
            return None;
        }
        IrConst::Float(value) => IrConst::Float(-value),
        IrConst::Double(value) => IrConst::Double(-value),
        IrConst::Boolean(_) | IrConst::Char(_) | IrConst::String(_) | IrConst::Null => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_numeric_negation_folds_without_reinterpreting_the_source() {
        assert_eq!(negate(&IrConst::Int(9)), Some(IrConst::Int(-9)));
        assert_eq!(negate(&IrConst::Boolean(true)), None);
    }
}
