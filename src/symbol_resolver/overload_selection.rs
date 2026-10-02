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
    /// `FunctionInfo` alone is ~1.2 KiB and is what makes this variant large; the rest of the
    /// tuple is a `Vec` and two `Copy` `Ty`s, so only the callable is boxed.
    Selected((Box<FunctionInfo>, Vec<Ty>, Ty, Ty)),
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

/// Compare fixed-arity shapes with vararg element shapes in one specificity pass.
///
/// Element expansion is included only when some candidate already matches argument
/// for argument. An empty call then still falls through to defaulted parameters
/// instead of selecting an empty vararg. When the shapes are equally specific, a
/// declaration without a vararg wins. An empty supplied shape is exact only when
/// the declaration itself has no value parameters. An exact tie that still carries
/// the vararg's array type is resolved here from the supplied element shapes.
pub(super) fn select_fixed_or_more_specific_vararg<'a>(
    candidates: &[(&'a FunctionInfo, Vec<Ty>)],
    args: &[CallArgKind],
    fits: impl Fn(usize, &Ty, &CallArgKind) -> bool,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> CandidateSelectionWithTies<&'a FunctionInfo> {
    let mut fixed = Vec::new();
    let mut elements = Vec::new();
    for (candidate, params) in candidates {
        if let Some(shape) = super::fixed_parameter_shape(params, args, &fits) {
            fixed.push((shape, *candidate));
        } else if candidate.call_sig.vararg {
            if let Some(shape) = super::candidate_vararg_shape(candidate, params, args, &fits) {
                elements.push((shape, *candidate));
            }
        }
    }
    // No fixed declaration accepted the call argument-for-argument. Leave element
    // expansion to the later vararg pass so an empty call still prefers a default.
    if fixed.is_empty() {
        return CandidateSelectionWithTies::None;
    }
    fixed.append(&mut elements);
    select_equally_specific(fixed, at_least_as_specific)
}

/// Most-specific parameter shapes, then the non-vararg tie-break.
///
/// The tie-break applies only when every maximal shape can forward to every
/// other. Incomparable parameter types stay ambiguous.
pub(super) fn select_equally_specific<'a>(
    shapes: Vec<(Vec<Ty>, &'a FunctionInfo)>,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> CandidateSelectionWithTies<&'a FunctionInfo> {
    let selection = unique_most_specific_with_conflicts_and_ties(
        shapes
            .iter()
            .map(|(shape, candidate)| (shape.clone(), *candidate)),
        &at_least_as_specific,
        |left, right| super::distinct_source_declarations(left, right),
    );
    if mutually_as_specific(&shapes, &selection, &at_least_as_specific) {
        prefer_non_vararg(selection)
    } else {
        selection
    }
}

fn mutually_as_specific<'a>(
    shapes: &[(Vec<Ty>, &'a FunctionInfo)],
    selection: &CandidateSelectionWithTies<&'a FunctionInfo>,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> bool {
    let CandidateSelectionWithTies::Ambiguous(candidates) = selection else {
        return false;
    };
    let maximal = shapes
        .iter()
        .filter(|(_, candidate)| {
            candidates
                .iter()
                .any(|tied| std::ptr::eq(*tied, *candidate))
        })
        .map(|(shape, _)| shape.as_slice())
        .collect::<Vec<_>>();
    maximal.iter().all(|left| {
        maximal.iter().all(|right| {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(*right)
                    .enumerate()
                    .all(|(position, (&left, &right))| at_least_as_specific(position, left, right))
        })
    })
}

fn prefer_non_vararg<'a>(
    selection: CandidateSelectionWithTies<&'a FunctionInfo>,
) -> CandidateSelectionWithTies<&'a FunctionInfo> {
    let CandidateSelectionWithTies::Ambiguous(candidates) = selection else {
        return selection;
    };
    let fixed = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.call_sig.vararg_index.is_none())
        .collect::<Vec<_>>();
    match fixed.len() {
        0 => CandidateSelectionWithTies::Ambiguous(candidates),
        1 => CandidateSelectionWithTies::Selected(fixed[0]),
        _ => CandidateSelectionWithTies::Ambiguous(fixed),
    }
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

#[cfg(test)]
mod tests {
    use super::ReceiverFunctionSelection;

    #[test]
    fn a_receiver_selection_carries_a_pointer_to_its_callable() {
        assert_eq!(std::mem::size_of::<ReceiverFunctionSelection>(), 96);
    }
}
