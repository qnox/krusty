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

/// Kotlin metadata's `HAS_CONSTANT` fact for an ordinary immutable property's checked initializer.
/// This does not make the property a source-level `const val`; it only describes the initializer
/// encoded in metadata.
///
/// A folded arithmetic result is still a constant here. Callers that want kotlinc's metadata rule
/// use [`metadata_constant_expression`], which rejects `1 + 2` and accepts a Java `val` field.
pub(super) fn is_metadata_constant(expression: &IrExpr) -> bool {
    matches!(
        expression,
        IrExpr::Const(
            IrConst::Boolean(_)
                | IrConst::Byte(_)
                | IrConst::Short(_)
                | IrConst::Int(_)
                | IrConst::Long(_)
                | IrConst::UByte(_)
                | IrConst::UShort(_)
                | IrConst::UInt(_)
                | IrConst::ULong(_)
                | IrConst::Float(_)
                | IrConst::Double(_)
                | IrConst::Char(_)
                | IrConst::String(_)
        )
    )
}

/// Whether `expression` has a constant value for metadata `HAS_CONSTANT`.
///
/// This follows kotlinc's `FirToConstantValueChecker`, not constant folding. A non-null literal, a
/// string template or a pre-resolution string-literal `+` chain whose every part qualifies, a
/// numeric conversion or unary plus/minus of such a value, and a read of a `const` property or a
/// Java `val` field all qualify. An ordinary `plus` (`1 + 2`, `constString + "b"`,
/// `File.separator + "z"`) does not, even when folding later produces a constant.
pub(super) fn metadata_constant_expression(
    body: &crate::fir::FirBody,
    index: &crate::fir::ResolvedModuleIndex,
    expression: crate::fir::FirExprId,
) -> bool {
    let Some(expression) = body.expr(expression) else {
        return false;
    };
    match &expression.kind {
        crate::fir::FirExprKind::Constant(constant) => {
            !matches!(constant, crate::fir::FirConstant::Null)
        }
        crate::fir::FirExprKind::StringTemplate(parts) => parts
            .iter()
            .all(|part| metadata_constant_expression(body, index, *part)),
        // An assignment conversion (`String!` to `String`, `Int` to `Long`, `String` to `Any`)
        // does not change whether the operand has a constant value. The property's own type
        // decides whether that value may be recorded.
        crate::fir::FirExprKind::ImplicitConversion { value, .. } => {
            metadata_constant_expression(body, index, *value)
        }
        crate::fir::FirExprKind::Unary { operation, operand }
            if matches!(
                operation,
                crate::fir::FirUnaryOperation::Negate | crate::fir::FirUnaryOperation::Identity
            ) =>
        {
            metadata_constant_expression(body, index, *operand)
        }
        crate::fir::FirExprKind::PropertyRead { target, .. } => {
            property_has_constant_value(index, target)
        }
        _ => false,
    }
}

fn property_has_constant_value(
    index: &crate::fir::ResolvedModuleIndex,
    target: &crate::fir::FirPropertyTarget,
) -> bool {
    match target {
        crate::fir::FirPropertyTarget::External {
            constant_value_field,
            ..
        } => *constant_value_field,
        crate::fir::FirPropertyTarget::Module { property, .. } => {
            index.property(*property).is_some_and(|header| {
                index.compile_time_constant(header.declaration).is_some()
                    || index
                        .declaration_header(header.declaration)
                        .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::CONST))
            })
        }
    }
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
