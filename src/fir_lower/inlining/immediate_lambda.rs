//! Expansion of a directly invoked, checked source lambda.

use super::super::{BodyLowering, FirLoweringFailure};
use super::{lambda_splice_contract, LambdaParameterBinding};
use crate::fir::OriginId;
use crate::ir::{ExprId, IrExpr};

pub(in super::super) fn splice_immediate_lambda(
    lowering: &mut BodyLowering<'_>,
    invocation: ExprId,
    origin: OriginId,
) -> Result<(), FirLoweringFailure> {
    let IrExpr::InvokeFunction { func, .. } = lowering.ir.expr(invocation) else {
        return Err(FirLoweringFailure::ImmediateLambdaExpansionDeclined { origin });
    };
    let IrExpr::Lambda {
        impl_fn,
        inline_body: Some(_),
        sam: None,
        ..
    } = lowering.ir.expr(*func)
    else {
        return Ok(());
    };
    if lowering.ir.suspend_funs.contains(impl_fn) {
        return Ok(());
    }
    lambda_splice_contract::preflight(lowering.ir, invocation, lowering.next_temporary)
        .ok_or(FirLoweringFailure::ImmediateLambdaExpansionDeclined { origin })?;
    lowering
        .splice_inline_lambda(
            invocation,
            LambdaParameterBinding::Declared { callee: None },
        )
        .ok_or(FirLoweringFailure::ImmediateLambdaExpansionDeclined { origin })?;
    Ok(())
}
