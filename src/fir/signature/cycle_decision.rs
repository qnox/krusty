//! Deciding a signature cycle: a committed re-entry into a declaration still being computed.

use super::{DiagnosticId, SignatureConstraintEvaluator, SignatureState};
use crate::fx_hash::HashMap;

use super::DeclarationId;

/// `declaration`'s evaluation committed to `target`, which is still being computed: every
/// declaration on the stack from `target` up is one loop, and each is a recursive problem. Returns
/// `declaration`'s own diagnostic. `target` finished in between only if the stack was unwound past
/// it, which an in-progress outcome cannot survive, so it names `declaration` alone then.
pub(super) fn decide_cycle(
    states: &mut HashMap<DeclarationId, SignatureState>,
    computing: &[DeclarationId],
    declaration: DeclarationId,
    target: DeclarationId,
    evaluator: &impl SignatureConstraintEvaluator,
) -> DiagnosticId {
    crate::trace_compiler!(
        "fir",
        "signature cycle: {declaration:?} committed to {target:?} with stack {computing:?}",
    );
    let cycle_start = computing
        .iter()
        .position(|candidate| *candidate == target)
        .unwrap_or_else(|| computing.len() - 1);
    for &member in &computing[cycle_start..] {
        let diagnostic = evaluator.recursive_inference_diagnostic(member);
        states.insert(member, SignatureState::Failed(diagnostic));
    }
    evaluator.recursive_inference_diagnostic(declaration)
}
