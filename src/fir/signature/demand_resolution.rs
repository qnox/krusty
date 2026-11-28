//! Resolving one declaration's signature on demand: a dependency is solved when first read, and a
//! re-entry into a declaration still being computed is decided as a cycle.

use super::{
    DiagnosticId, ResolvedSignature, SignatureConstraintEvaluator, SignatureGraph, SignatureState,
};
use crate::fx_hash::HashMap;

use super::DeclarationId;

pub(super) fn resolve_signature(
    graph: &SignatureGraph,
    states: &mut HashMap<DeclarationId, SignatureState>,
    computing: &mut Vec<DeclarationId>,
    declaration: DeclarationId,
    evaluator: &impl SignatureConstraintEvaluator,
) -> Result<ResolvedSignature, DiagnosticId> {
    match states.get(&declaration) {
        Some(SignatureState::Resolved(signature)) => return Ok(signature.clone()),
        Some(SignatureState::Failed(diagnostic)) => return Err(*diagnostic),
        // Re-entry is not a decision: a declaration still being computed has no known type. A
        // caller that merely probes it may move on; one that commits to it decides the cycle in
        // `decide_cycle` below.
        Some(SignatureState::Computing) => return Err(DiagnosticId::in_progress(declaration)),
        Some(SignatureState::Uncomputed) | None => {}
    }

    let Some(constraint) = graph.constraint(declaration) else {
        crate::trace_compiler!(
            "fir",
            "signature solve: {declaration:?} has no extracted constraint",
        );
        let diagnostic = evaluator.missing_signature_diagnostic(declaration);
        states.insert(declaration, SignatureState::Failed(diagnostic));
        return Err(diagnostic);
    };
    states.insert(declaration, SignatureState::Computing);
    computing.push(declaration);
    let evaluated = evaluator.evaluate(declaration, constraint.result, graph, &mut |dependency| {
        resolve_signature(graph, states, computing, dependency, evaluator)
    });
    let evaluated = evaluated.map_err(|failure| match failure.in_progress_declaration() {
        Some(target) => {
            super::cycle_decision::decide_cycle(states, computing, declaration, target, evaluator)
        }
        None => failure,
    });
    let popped = computing.pop();
    debug_assert_eq!(popped, Some(declaration));

    // A committed re-entry already marked this declaration as a cycle member. Never overwrite that
    // diagnostic with a value computed on top of the recursive failure.
    if let Some(SignatureState::Failed(diagnostic)) = states.get(&declaration) {
        return Err(*diagnostic);
    }
    match evaluated {
        Ok(signature) => {
            crate::trace_compiler!(
                "fir",
                "signature resolved declaration={declaration:?} parameters={:?} result={:?}",
                signature.parameters,
                signature.result,
            );
            states.insert(declaration, SignatureState::Resolved(signature.clone()));
            Ok(signature)
        }
        Err(diagnostic) => {
            crate::trace_compiler!(
                "fir",
                "signature evaluation failed for {declaration:?} (constraint result {:?} = {:?})",
                constraint.result,
                graph.expr(constraint.result),
            );
            states.insert(declaration, SignatureState::Failed(diagnostic));
            Err(diagnostic)
        }
    }
}
