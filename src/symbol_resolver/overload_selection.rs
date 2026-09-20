//! Shared overload-selection outcomes and maximal-candidate retention.
//!
//! Ordinary calls normally need only selected/none/ambiguous. Diagnostics for conventions need
//! the exact tied maximal declarations, in provider order, without repeating candidate lookup.

use super::{declaration_specificity, CallArgKind, CandidateSelection, FunctionInfo, Ty};

pub(crate) enum CandidateSelectionWithTies<T> {
    None,
    Selected(T),
    Ambiguous(Vec<T>),
}

pub(crate) enum ReceiverFunctionSelection {
    None,
    Selected((FunctionInfo, Vec<Ty>, Ty, Ty)),
    Ambiguous(Vec<FunctionInfo>),
}

impl<T> CandidateSelectionWithTies<T> {
    pub(super) fn collapse(self) -> CandidateSelection<T> {
        match self {
            Self::None => CandidateSelection::None,
            Self::Selected(selected) => CandidateSelection::Selected(selected),
            Self::Ambiguous(_) => CandidateSelection::Ambiguous,
        }
    }
}

pub(super) fn unique_most_specific<T>(
    candidates: impl IntoIterator<Item = (Vec<Ty>, T)>,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> CandidateSelection<T> {
    unique_most_specific_with_conflicts(candidates, at_least_as_specific, |_, _| false)
}

pub(super) fn unique_most_specific_with_conflicts<T>(
    candidates: impl IntoIterator<Item = (Vec<Ty>, T)>,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
    equivalent_conflicts: impl Fn(&T, &T) -> bool,
) -> CandidateSelection<T> {
    unique_most_specific_with_conflicts_and_ties(
        candidates,
        at_least_as_specific,
        equivalent_conflicts,
    )
    .collapse()
}

pub(super) fn unique_most_specific_with_conflicts_and_ties<T>(
    candidates: impl IntoIterator<Item = (Vec<Ty>, T)>,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
    equivalent_conflicts: impl Fn(&T, &T) -> bool,
) -> CandidateSelectionWithTies<T> {
    let mut applicable = Vec::new();
    for (params, candidate) in candidates {
        let equivalent =
            applicable.iter().find(|(existing, _): &&(Vec<Ty>, T)| {
                existing.len() == params.len()
                    && existing.iter().zip(&params).enumerate().all(
                        |(position, (&left, &right))| {
                            at_least_as_specific(position, left, right)
                                && at_least_as_specific(position, right, left)
                        },
                    )
            });
        if let Some((_, existing_candidate)) = equivalent {
            if !equivalent_conflicts(existing_candidate, &candidate) {
                continue;
            }
        }
        applicable.push((params, candidate));
    }
    if applicable.is_empty() {
        return CandidateSelectionWithTies::None;
    }

    let parameter_shapes = applicable
        .iter()
        .map(|(parameters, _)| parameters.as_slice())
        .collect::<Vec<_>>();
    let most_specific =
        declaration_specificity::most_specific_indices(&parameter_shapes, at_least_as_specific);
    let [selected] = most_specific.as_slice() else {
        return CandidateSelectionWithTies::Ambiguous(
            applicable
                .into_iter()
                .enumerate()
                .filter_map(|(index, (_, candidate))| {
                    most_specific.contains(&index).then_some(candidate)
                })
                .collect(),
        );
    };
    CandidateSelectionWithTies::Selected(applicable.swap_remove(*selected).1)
}

fn integer_literal_call_applies(
    params: &[Ty],
    args: &[CallArgKind],
    mut fits: impl FnMut(usize, &Ty, &CallArgKind) -> bool,
) -> Option<bool> {
    if params.len() != args.len() {
        return None;
    }
    params
        .iter()
        .zip(args)
        .enumerate()
        .try_fold(false, |adapted, (i, (&param, arg))| {
            if param == arg.ty() {
                Some(adapted)
            } else if arg.adapts_integer_literal_to(param) {
                Some(true)
            } else if fits(i, &param, arg) {
                Some(adapted)
            } else {
                None
            }
        })
}

pub(super) fn integer_literal_overload<T>(
    candidates: impl Iterator<Item = (Vec<Ty>, T)>,
    args: &[CallArgKind],
    fits: impl FnMut(usize, &Ty, &CallArgKind) -> bool,
    at_least_as_specific: impl Fn(usize, Ty, Ty, CallArgKind) -> bool,
    equivalent_conflicts: impl Fn(&T, &T) -> bool,
) -> CandidateSelection<T> {
    integer_literal_overload_with_ties(
        candidates,
        args,
        fits,
        at_least_as_specific,
        equivalent_conflicts,
    )
    .collapse()
}

pub(super) fn integer_literal_overload_with_ties<T>(
    candidates: impl Iterator<Item = (Vec<Ty>, T)>,
    args: &[CallArgKind],
    mut fits: impl FnMut(usize, &Ty, &CallArgKind) -> bool,
    at_least_as_specific: impl Fn(usize, Ty, Ty, CallArgKind) -> bool,
    equivalent_conflicts: impl Fn(&T, &T) -> bool,
) -> CandidateSelectionWithTies<T> {
    if !args.iter().any(|arg| arg.is_integer_literal()) {
        return CandidateSelectionWithTies::None;
    }
    let mut applicable = Vec::new();
    let mut has_adaptation = false;
    for (params, candidate) in candidates {
        let Some(adapted) = integer_literal_call_applies(&params, args, &mut fits) else {
            continue;
        };
        has_adaptation |= adapted;
        if let Some((_, existing_candidate)) = applicable
            .iter()
            .find(|(existing, _): &&(Vec<Ty>, T)| existing == &params)
        {
            if !equivalent_conflicts(existing_candidate, &candidate) {
                continue;
            }
        }
        applicable.push((params, candidate));
    }
    if !has_adaptation {
        return CandidateSelectionWithTies::None;
    }
    unique_most_specific_with_conflicts_and_ties(
        applicable,
        |position, left, right| {
            at_least_as_specific(
                position,
                left,
                right,
                args.get(position)
                    .unwrap_or(&CallArgKind::Typed(Ty::Error))
                    .clone(),
            )
        },
        equivalent_conflicts,
    )
}
