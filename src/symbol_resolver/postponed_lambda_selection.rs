//! A lambda literal's result in overload selection.
//!
//! Kotlin decides applicability and most-specific selection without analyzing lambda bodies, so a
//! lambda's checked result takes part only where the specification lets it. The rule is the same
//! whatever argument form a family is compared in, a shared positional mapping or each
//! declaration's own, so both selectors run it from here.

use super::*;
use crate::types::TyKind;

/// Whether a lambda literal's checked result is still postponed or already takes part.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LambdaResults {
    Postponed,
    Checked,
}

/// Select among `candidates` the way Kotlin treats a lambda literal's result, whatever the
/// argument form the family is compared in. Kotlin decides applicability and most-specific
/// selection without analyzing lambda bodies, so a lambda literal's result first takes part only
/// as a postponed probe. The checked result refines a tie only where the specification's
/// lambda-return-type refinement applies ([`lambda_result_refines`]); every other tie is an
/// ambiguity, and a unique most specific candidate keeps its own result expectation for the
/// lambda. Under `+EagerLambdaAnalysis` (`eager_lambda_results`) the checked result instead takes
/// part in applicability.
///
/// `args` are the call's arguments in the positions `lambda_parameter` reads: it returns the
/// parameter a candidate (by index) takes at an argument position. `select` chooses among the
/// candidates at the given indices and answers in those indices.
pub(super) fn select_with_postponed_lambda_results(
    candidates: &[&FunctionInfo],
    args: &[CallArgKind],
    eager_lambda_results: bool,
    lambda_parameter: impl Fn(usize, usize) -> Option<Ty>,
    select: impl Fn(&[usize], LambdaResults) -> CandidateSelectionWithTies<usize>,
) -> CandidateSelectionWithTies<usize> {
    let all = (0..candidates.len()).collect::<Vec<_>>();
    if eager_lambda_results
        || args
            .iter()
            .all(|argument| argument.with_postponed_lambda_result().ty() == argument.ty())
    {
        return select(&all, LambdaResults::Checked);
    }
    let mut tied = match select(&all, LambdaResults::Postponed) {
        CandidateSelectionWithTies::Ambiguous(tied) => tied,
        CandidateSelectionWithTies::Selected(selected) => {
            // The selection is proven only when the lambda's checked result fits the selected
            // candidate. Otherwise it stays with the body check, which reports the lambda
            // against that candidate.
            return match select(&[selected], LambdaResults::Checked) {
                CandidateSelectionWithTies::Selected(_) => {
                    CandidateSelectionWithTies::Selected(selected)
                }
                _ => CandidateSelectionWithTies::None,
            };
        }
        CandidateSelectionWithTies::None => return CandidateSelectionWithTies::None,
    };
    tied.sort_unstable();
    tied.dedup();
    if !lambda_result_refines(candidates, &tied, args, &lambda_parameter) {
        return CandidateSelectionWithTies::Ambiguous(tied);
    }
    match select(&tied, LambdaResults::Checked) {
        CandidateSelectionWithTies::Ambiguous(refined) => {
            // A refined tie prefers the candidates not annotated for refinement.
            let plain = refined
                .iter()
                .copied()
                .filter(|&candidate| !refines_by_lambda_result(candidates[candidate]))
                .collect::<Vec<_>>();
            match plain.as_slice() {
                [selected] => CandidateSelectionWithTies::Selected(*selected),
                [] => CandidateSelectionWithTies::Ambiguous(refined),
                _ => CandidateSelectionWithTies::Ambiguous(plain),
            }
        }
        decided => decided,
    }
}

fn refines_by_lambda_result(candidate: &FunctionInfo) -> bool {
    candidate
        .callable
        .annotations
        .contains(&crate::types::type_name(
            "kotlin/OverloadResolutionByLambdaReturnType",
        ))
}

/// Whether a most-specific tie may be refined by its lambda's checked result: some tied candidate
/// is `@OverloadResolutionByLambdaReturnType`, the call has exactly one lambda literal, and every
/// tied candidate's parameter for it is a function type with the same inputs.
fn lambda_result_refines(
    candidates: &[&FunctionInfo],
    tied: &[usize],
    args: &[CallArgKind],
    lambda_parameter: &impl Fn(usize, usize) -> Option<Ty>,
) -> bool {
    let mut lambdas = args
        .iter()
        .enumerate()
        .filter(|(_, argument)| argument.is_lambda_literal())
        .map(|(position, _)| position);
    let (Some(lambda), None) = (lambdas.next(), lambdas.next()) else {
        return false;
    };
    let inputs = |candidate: usize| match lambda_parameter(candidate, lambda)?.non_null().kind() {
        TyKind::Fun(signature) => Some((
            signature.params.clone(),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        )),
        _ => None,
    };
    let Some((&first, others)) = tied.split_first() else {
        return false;
    };
    tied.iter()
        .any(|&candidate| refines_by_lambda_result(candidates[candidate]))
        && inputs(first).is_some_and(|first| {
            others
                .iter()
                .all(|&candidate| inputs(candidate).as_ref() == Some(&first))
        })
}
