//! Shrinking parser-era adapter for unguarded inline plans.
//!
//! Checked FIR consumes the complete contract directly. The legacy AST lowerer can represent only
//! the original lambda-only subset and must reject every generalized plan without retrying it as an
//! ordinary dependency call.

use crate::libraries::{InlineBodyPlan, InlineBodyValue};

pub(super) fn plain_invoke_lambda(
    plan: &InlineBodyPlan,
) -> Option<(usize, Vec<usize>, Option<usize>)> {
    let InlineBodyPlan::InvokeLambda {
        lambda_parameter,
        arguments,
        prologue,
        cleanup,
        cause,
        defaults,
        result,
    } = plan
    else {
        return None;
    };
    if !prologue.is_empty() || !cleanup.is_empty() || cause.is_some() || !defaults.is_empty() {
        return None;
    }
    let parameter = |value: &InlineBodyValue| match value {
        InlineBodyValue::Parameter(parameter) => Some(*parameter),
        InlineBodyValue::Cause => None,
    };
    Some((
        *lambda_parameter,
        arguments
            .iter()
            .map(parameter)
            .collect::<Option<Vec<_>>>()?,
        match result {
            None => None,
            Some(result) => Some(parameter(result)?),
        },
    ))
}
