//! Which checked expressions realize a source call as a builtin operation.

use crate::fir::{FirBody, FirCallTarget, FirConversionKind, FirExpr, FirExprId, FirExprKind};
use crate::ir::{IrBuiltinCall, IrBuiltinCallKind};

/// The builtin call `expression` makes, with its checked result and operand types: an operator, a
/// numeric conversion, an indexed read, or a member call selected as a compiler intrinsic. Common
/// IR keeps it as provenance; how a target realizes the call is that target's decision.
pub(super) fn builtin_call(body: &FirBody, expression: &FirExpr) -> Option<IrBuiltinCall> {
    let ty = |operand: FirExprId| body.expr(operand).map(|operand| operand.ty.get());
    let (kind, operands) = match &expression.kind {
        FirExprKind::Unary { operation, operand } => {
            (IrBuiltinCallKind::Unary(*operation), vec![ty(*operand)?])
        }
        FirExprKind::Binary {
            operation,
            lhs,
            rhs,
        } => (
            IrBuiltinCallKind::Binary(*operation),
            vec![ty(*lhs)?, ty(*rhs)?],
        ),
        FirExprKind::ImplicitConversion { value, conversion }
            if matches!(conversion.kind, FirConversionKind::NumericConversion { .. }) =>
        {
            (IrBuiltinCallKind::NumericConversion, vec![ty(*value)?])
        }
        FirExprKind::IndexedRead { .. } => (IrBuiltinCallKind::IndexedRead, Vec::new()),
        FirExprKind::Call(call) if matches!(call.target, FirCallTarget::Intrinsic { .. }) => {
            (IrBuiltinCallKind::Intrinsic, Vec::new())
        }
        _ => return None,
    };
    Some(IrBuiltinCall {
        kind,
        result: expression.ty.get(),
        operands,
    })
}
