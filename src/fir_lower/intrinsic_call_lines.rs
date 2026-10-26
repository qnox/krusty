//! Which checked expressions are kotlinc intrinsic calls that state their own line first.

use crate::fir::{
    FirBinaryOperation, FirBody, FirCallTarget, FirConversionKind, FirExpr, FirExprId, FirExprKind,
    FirIntrinsic, FirUnaryOperation,
};

/// Whether kotlinc's JVM codegen realizes `expression` as an intrinsic call, which marks the
/// call's own line before it generates any operand.
///
/// When the receiver or first operand starts on another line (`h(\n a,\n b,\n).size`), that operand
/// marks its line at the same offset, and kotlinc's line table keeps both entries. An ordinary call
/// (`String.length`, a property getter, `Enum.name`, `toString()`) marks its line only at its
/// invoke, after its operands. So does an inline one: a conversion that realizes an inlined getter
/// (`Char.code`), and every unsigned operator and conversion, which the standard library declares
/// `inline` over the signed carrier.
pub(super) fn states_line_before_operands(body: &FirBody, expression: &FirExpr) -> bool {
    match &expression.kind {
        FirExprKind::Unary { operation, operand } => {
            !matches!(operation, FirUnaryOperation::Identity)
                && signed(expression)
                && signed_operand(body, *operand)
        }
        FirExprKind::Binary {
            operation,
            lhs,
            rhs,
        } => {
            !matches!(
                operation,
                FirBinaryOperation::BooleanAnd | FirBinaryOperation::BooleanOr
            ) && signed(expression)
                && signed_operand(body, *lhs)
                && signed_operand(body, *rhs)
        }
        // A widening or narrowing between two primitives; an unboxing is no intrinsic call.
        FirExprKind::ImplicitConversion { value, conversion } => {
            let FirConversionKind::NumericConversion { to } = conversion.kind else {
                return false;
            };
            let from = body.expr(*value).map(|value| value.ty.get());
            !body.is_inlined_call_conversion(conversion.origin)
                && from.is_some_and(|from| {
                    from.is_numeric_or_char() && to.get().is_numeric_or_char() && from != to.get()
                })
                && signed(expression)
                && signed_operand(body, *value)
        }
        FirExprKind::IndexedRead { .. } => true,
        FirExprKind::Call(call) => matches!(
            call.target,
            FirCallTarget::Intrinsic {
                operation: FirIntrinsic::ArraySize
                    | FirIntrinsic::ArrayGet
                    | FirIntrinsic::StringGet
                    | FirIntrinsic::PrimitiveCompare { .. },
                ..
            }
        ),
        _ => false,
    }
}

fn signed(expression: &FirExpr) -> bool {
    !expression
        .ty
        .get()
        .non_null()
        .canonical_semantic()
        .is_unsigned()
}

fn signed_operand(body: &FirBody, operand: FirExprId) -> bool {
    body.expr(operand).is_some_and(signed)
}
