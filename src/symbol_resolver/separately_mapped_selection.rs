//! Overload selection over a family whose declarations map a call's source arguments differently.
//!
//! A family is selected under one argument list when every declaration maps the source arguments
//! to the same parameters. When the declarations name or omit different parameters there is no
//! shared list, and each declaration carries its own mapping ([`CandidateCall`]). The declarations
//! are then compared in source-argument order, through each one's own mapping. A top-level family
//! and a receiver family (the members and extensions at one tower rank) are both selected here.

use super::declaration_specificity::discriminate_generics;
use super::overload_selection::select_by_priority_tier;
use super::postponed_lambda_selection::{select_with_postponed_lambda_results, LambdaResults};
use super::*;

/// One declaration's own mapping of a call's source arguments.
pub(crate) struct CandidateCall {
    /// The arguments in value-parameter order; a parameter left to its default holds
    /// [`CallArgKind::OmittedDefault`].
    pub(crate) args: Vec<CallArgKind>,
    /// The declaration parameter each source argument maps to.
    pub(crate) parameter_by_argument: Box<[Option<u32>]>,
}

/// The argument list a callable family is selected under.
#[derive(Clone, Copy)]
pub(crate) enum FamilyArguments<'a> {
    /// One mapping shared by every declaration, in parameter order.
    Shared(&'a [CallArgKind]),
    /// Each declaration's own mapping, parallel to the family, and the source arguments in
    /// source order.
    PerCandidate {
        calls: &'a [CandidateCall],
        source: &'a [CallArgKind],
    },
}

/// One declaration of a separately mapped family, instantiated for the call.
#[derive(Clone, Copy)]
pub(super) struct SeparatelyMapped<'p> {
    pub(super) function: &'p FunctionInfo,
    /// Its value parameters, instantiated for the call.
    pub(super) params: &'p [Ty],
    /// Its own arguments in value-parameter order.
    pub(super) args: &'p [CallArgKind],
    pub(super) call: &'p CandidateCall,
}

impl SeparatelyMapped<'_> {
    /// The parameter the source argument at `position` maps to.
    fn parameter_for_source(&self, position: usize) -> Option<Ty> {
        let parameter = usize::try_from((*self.call.parameter_by_argument.get(position)?)?).ok()?;
        self.params
            .get(parameter.checked_sub(self.function.context_count)?)
            .copied()
    }

    /// `shape`, parallel to this declaration's value-parameter-ordered arguments, read in
    /// source-argument order through its own mapping.
    fn source_order_shape(&self, shape: &[Ty]) -> Option<Vec<Ty>> {
        if shape.len() != self.args.len() {
            return None;
        }
        let context = self.function.context_count;
        self.call
            .parameter_by_argument
            .iter()
            .map(|parameter| {
                let parameter = usize::try_from((*parameter)?).ok()?;
                shape.get(parameter.checked_sub(context)?).copied()
            })
            .collect()
    }
}

/// Select from a separately mapped family, returning indices into `entries`. A lambda literal's
/// result takes part as it does for a shared mapping ([`select_with_postponed_lambda_results`]),
/// and `@LowPriorityInOverloadResolution` declarations compete only when no other one applies.
pub(super) fn select_separately_mapped(
    src: &dyn SymbolSource,
    entries: &[SeparatelyMapped<'_>],
    source: &[CallArgKind],
    eager_lambda_results: bool,
    fits: &dyn Fn(&Ty, &CallArgKind) -> bool,
    adapts: &dyn Fn(&Ty, &CallArgKind, usize) -> bool,
) -> CandidateSelectionWithTies<usize> {
    let functions = entries
        .iter()
        .map(|entry| entry.function)
        .collect::<Vec<_>>();
    let postponed = |args: &[CallArgKind]| {
        args.iter()
            .map(CallArgKind::with_postponed_lambda_result)
            .collect::<Vec<_>>()
    };
    let postponed_args = entries
        .iter()
        .map(|entry| postponed(entry.args))
        .collect::<Vec<_>>();
    let postponed_source = postponed(source);
    select_with_postponed_lambda_results(
        &functions,
        source,
        eager_lambda_results,
        |candidate, position| entries[candidate].parameter_for_source(position),
        |indices, results| {
            let subset = indices
                .iter()
                .map(|&index| SeparatelyMapped {
                    args: match results {
                        LambdaResults::Postponed => &postponed_args[index],
                        LambdaResults::Checked => entries[index].args,
                    },
                    ..entries[index]
                })
                .collect::<Vec<_>>();
            let source = match results {
                LambdaResults::Postponed => &postponed_source,
                LambdaResults::Checked => source,
            };
            select_by_priority_tier(
                &(0..subset.len()).collect::<Vec<_>>(),
                |position| subset[position].function,
                |tier| {
                    let tier_entries = tier
                        .iter()
                        .map(|&position| subset[position])
                        .collect::<Vec<_>>();
                    most_specific_separately_mapped(src, &tier_entries, source, fits, adapts)
                        .map(|position| tier[position])
                },
            )
            .map(|position| indices[position])
        },
    )
}

/// A family whose declarations map the source arguments differently. Each declaration's
/// applicable shape is read in source-argument order (a parameter left to its default takes
/// no part), and the declarations are compared there exactly as a shared family is: integer
/// literal adaptation first, then specificity. Declarations whose compared parameters are
/// identical are separated first by a vararg (the one without wins), then by the defaults
/// they leave out (the fewest win); distinct declarations still equal after that are ambiguous.
fn most_specific_separately_mapped(
    src: &dyn SymbolSource,
    entries: &[SeparatelyMapped<'_>],
    source: &[CallArgKind],
    fits: &dyn Fn(&Ty, &CallArgKind) -> bool,
    adapts: &dyn Fn(&Ty, &CallArgKind, usize) -> bool,
) -> CandidateSelectionWithTies<usize> {
    let mut shaped = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let args = entry.args;
        let shape = fixed_parameter_shape(entry.params, args, |_, param, arg| fits(param, arg))
            .or_else(|| {
                candidate_vararg_shape(entry.function, entry.params, args, |i, param, arg| {
                    fits(param, arg) || adapts(param, arg, i)
                })
            })
            .or_else(|| {
                fixed_parameter_shape(entry.params, args, |i, param, arg| {
                    fits(param, arg) || adapts(param, arg, i)
                })
            });
        let Some(shape) = shape else {
            continue;
        };
        // Every applicable declaration must be comparable; one whose shape cannot be read in
        // source order leaves the call without a defensible selection.
        let Some(projected) = entry.source_order_shape(&shape) else {
            return CandidateSelectionWithTies::Ambiguous((0..entries.len()).collect());
        };
        let omitted = entry.params.len().saturating_sub(args.len())
            + args.iter().filter(|arg| arg.is_omitted_default()).count();
        shaped.push((projected, index, omitted));
    }
    let preferred = shaped
        .iter()
        .filter(|(shape, index, omitted)| {
            !shaped.iter().any(|(other_shape, other, other_omitted)| {
                other_shape == shape
                    && (entries[*index].function.call_sig.vararg, *omitted)
                        > (entries[*other].function.call_sig.vararg, *other_omitted)
            })
        })
        .map(|(shape, index, _)| (shape.clone(), *index))
        .collect::<Vec<_>>();
    // Separate mappings can project different declarations onto one shape; kotlinc then reports
    // the ambiguity between them rather than selecting either.
    let distinct_declarations = |left: &usize, right: &usize| {
        !std::ptr::eq(entries[*left].function, entries[*right].function)
    };
    let generic = |index: usize| {
        !entries[index]
            .function
            .semantic_signature()
            .formals
            .is_empty()
    };
    match discriminate_generics(
        integer_literal_overload_with_ties(
            preferred.iter().cloned(),
            source,
            |_, param, arg| fits(param, arg),
            |_position, left, right, arg| {
                parameter_at_least_as_specific(src, left, right, arg)
                    || resolution_subtype(src, left, right)
            },
            distinct_declarations,
        ),
        generic,
    ) {
        CandidateSelectionWithTies::None => {}
        selection => return selection,
    }
    discriminate_generics(
        unique_most_specific_with_conflicts_and_ties(
            preferred,
            |position, left, right| {
                source.get(position).is_some_and(|argument| {
                    parameter_at_least_as_specific(src, left, right, argument.clone())
                })
            },
            distinct_declarations,
        ),
        generic,
    )
}
