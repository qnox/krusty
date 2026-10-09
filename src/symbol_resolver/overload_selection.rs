//! Shared overload-selection outcomes and maximal-candidate retention.
//!
//! Ordinary calls normally need only selected/none/ambiguous. Diagnostics for conventions need
//! the exact tied maximal declarations, in provider order, without repeating candidate lookup.

use super::*;

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
        // A contextual nested call may bind to both the raw vararg array and its element. The
        // source call supplies an element unless the recorded vararg mapper recognizes a true
        // whole-array/spread form, so classify the declared vararg shape first. Otherwise the raw
        // array can hide a narrower element before specificity runs.
        if candidate.call_sig.vararg {
            if let Some(shape) = super::candidate_vararg_shape(candidate, params, args, &fits) {
                elements.push((shape, *candidate));
                continue;
            }
        }
        if let Some(shape) = super::fixed_parameter_shape(params, args, &fits) {
            fixed.push((shape, *candidate));
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

/// Select an exact argument-for-parameter shape only from semantic types fixed by the argument's
/// own inputs. A result-only contextual producer has no exact provisional type: its enclosing
/// parameter shapes it, so it must reach the ordinary fixed/vararg declaration comparison.
pub(super) fn select_recorded_exact<'a>(
    candidates: &[(&'a FunctionInfo, Vec<Ty>)],
    args: &[CallArgKind],
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> CandidateSelectionWithTies<&'a FunctionInfo> {
    if args.iter().any(|argument| {
        argument.is_expected_type_callable() && !argument.result_is_input_constrained()
    }) {
        return CandidateSelectionWithTies::None;
    }
    let actual = args.iter().map(CallArgKind::ty).collect::<Vec<_>>();
    select_equally_specific(
        candidates
            .iter()
            .filter_map(|(candidate, parameters)| {
                let declared = declaration_specificity_params(candidate);
                (parameters.as_slice() == actual
                    && (!args.is_empty() || declared.len() == parameters.len()))
                .then_some((declared, *candidate))
            })
            .collect(),
        at_least_as_specific,
    )
}

/// Most-specific parameter shapes, then the non-vararg tie-break.
///
/// The tie-break applies only when every maximal shape can forward to every
/// other. Incomparable parameter types stay ambiguous. A library pair stays in
/// the set when only one candidate is a vararg, so equal shapes such as
/// `listOf(element)` and `listOf(vararg)` still reach that tie-break.
/// Integer adaptation has chosen these maxima. Equal shapes still prefer the
/// declaration without a vararg; incomparable adapted shapes stay ambiguous.
pub(super) fn non_vararg_among<'a>(
    candidates: &[(&'a FunctionInfo, Vec<Ty>)],
    tied: &[&'a FunctionInfo],
    src: &dyn crate::symbol_source::SymbolSource,
) -> CandidateSelectionWithTies<&'a FunctionInfo> {
    select_equally_specific(
        candidates
            .iter()
            .filter(|(candidate, _)| tied.iter().any(|tied| std::ptr::eq(*tied, *candidate)))
            .map(|(candidate, params)| (params.clone(), *candidate))
            .collect(),
        |_, left, right| {
            super::parameter_at_least_as_specific(src, left, right, CallArgKind::Typed(Ty::Error))
        },
    )
}

pub(super) fn select_equally_specific(
    shapes: Vec<(Vec<Ty>, &FunctionInfo)>,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> CandidateSelectionWithTies<&FunctionInfo> {
    let selection = unique_most_specific_with_conflicts_and_ties(
        shapes
            .iter()
            .map(|(shape, candidate)| (shape.clone(), *candidate)),
        &at_least_as_specific,
        |left, right| {
            distinct_source_declarations(left, right)
                || left.call_sig.vararg_index.is_some() != right.call_sig.vararg_index.is_some()
        },
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

fn prefer_non_vararg(
    selection: CandidateSelectionWithTies<&FunctionInfo>,
) -> CandidateSelectionWithTies<&FunctionInfo> {
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

/// Extension-selection context for [`select_overload`]: whether non-public `@InlineOnly` candidates are
/// admitted (the bytecode inliner), and the packages in scope for an extension (`None` = unscoped). Both
/// only affect EXTENSION selection — a member is always visible on its type.
#[derive(Clone, Copy)]
pub(super) struct ExtCtx<'a> {
    pub(super) fn_scope: Option<FunctionScopeRef<'a>>,
    pub(super) source: &'a dyn SymbolSource,
    pub(super) type_variables: &'a [String],
}

/// The single call-overload selector for a receiver call `recv.name(args)`. It is parameterized by
/// [`FnKind`] — MEMBER and EXTENSION resolution differ only in the *calling convention* the backend emits
/// (invokevirtual with `this` vs invokestatic with the receiver as the leading arg), NOT in how the best
/// overload is chosen. The receiver is always an ATTRIBUTE, never `params[0]`: candidates are matched
/// against their LOGICAL value parameters (a member's `callable.params` are value-only; an extension's
/// prepend the receiver in the JVM emit shape, so [`logical_value_params`] strips it). Overloads are tried
/// closest-receiver-rank first, and within a rank by the ordered applicability passes below.
pub(super) fn select_overload(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
    kind: FnKind,
    ext: ExtCtx<'_>,
) -> Option<FunctionInfo> {
    let mut ambiguous = false;
    select_overload_tracking(lib, recv, name, args, type_args, kind, ext, &mut ambiguous)
}

#[derive(Clone, Copy, Debug)]
pub(super) enum SelectionMode {
    Kind(FnKind),
    Receiver,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IndexedConvention {
    Ordinary,
    Get,
    Set,
}

/// Project a fixed-arity indexed convention onto the operands written by indexed syntax. For
/// `set(i1, i2 = default, value)`, `receiver[i1] = value` maps to parameters `[0, 2]`; ordinary
/// positional mapping cannot express the required value after an omitted defaulted index.
fn indexed_fixed_parameter_projection(
    candidate: &FunctionInfo,
    params: &[Ty],
    argument_count: usize,
    convention: IndexedConvention,
) -> Option<Vec<Ty>> {
    let set = convention == IndexedConvention::Set;
    let parameter_indices = if set {
        let index_count = argument_count.checked_sub(1)?;
        if index_count >= params.len() {
            return None;
        }
        (0..index_count)
            .chain(std::iter::once(params.len() - 1))
            .collect::<Vec<_>>()
    } else {
        if argument_count > params.len() {
            return None;
        }
        (0..argument_count).collect::<Vec<_>>()
    };
    let defaults = candidate
        .call_sig
        .param_defaults
        .get(candidate.context_count..)
        .unwrap_or_default();
    if (0..params.len()).any(|parameter| {
        !parameter_indices.contains(&parameter)
            && !defaults.get(parameter).copied().unwrap_or(false)
    }) {
        return None;
    }
    Some(
        parameter_indices
            .into_iter()
            .map(|parameter| params[parameter])
            .collect(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn select_receiver_overload_from_functions_tracking(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
    ext: ExtCtx<'_>,
    functions: &[FunctionInfo],
    indexed: IndexedConvention,
) -> CandidateSelectionWithTies<FunctionInfo> {
    let mut ambiguous = false;
    let mut ambiguous_candidates = Vec::new();
    let selected = select_overload_tracking_with_functions(
        lib,
        recv,
        name,
        args,
        type_args,
        SelectionMode::Receiver,
        ext,
        Some(functions),
        &mut ambiguous,
        Some(&mut ambiguous_candidates),
        indexed,
    );
    if ambiguous {
        CandidateSelectionWithTies::Ambiguous(ambiguous_candidates)
    } else if let Some(selected) = selected {
        CandidateSelectionWithTies::Selected(selected)
    } else {
        CandidateSelectionWithTies::None
    }
}

fn select_overload_tracking(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
    kind: FnKind,
    ext: ExtCtx<'_>,
    ambiguous: &mut bool,
) -> Option<FunctionInfo> {
    select_overload_tracking_with_functions(
        lib,
        recv,
        name,
        args,
        type_args,
        SelectionMode::Kind(kind),
        ext,
        None,
        ambiguous,
        None,
        IndexedConvention::Ordinary,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn select_overload_tracking_with_functions(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
    mode: SelectionMode,
    ext: ExtCtx<'_>,
    provided_functions: Option<&[FunctionInfo]>,
    ambiguous: &mut bool,
    mut ambiguous_candidates: Option<&mut Vec<FunctionInfo>>,
    indexed: IndexedConvention,
) -> Option<FunctionInfo> {
    let src = ext.source;
    // Argument assignability and candidate enumeration consume the same federated source. Access
    // control is checked after selection from the declaration metadata carried by the winner.
    let assign_src = src;
    let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
    // EXTENSION candidates come from the ONE query — union `resolve_symbols`' function callables over the
    // in-scope packages (scope-pruned, tree-driven), so an unqualified extension binds only when its
    // facade's package is imported. No import scope → the whole-classpath `functions()` fallback
    // (removed once every consumer is scoped — task A). MEMBERS are always visible on their type.
    // A MEMBER's return can be RECEIVER-COUPLED (`Repo<Cfg>.byId(): Cfg`, a suspend `Continuation<T>`
    // bound from the receiver's type argument) — recovery the receiver-agnostic `resolve_type` cannot
    // do — so member candidates come from the platform's receiver-aware member query. EXTENSIONS come
    // from the scope-pruned `resolve_symbols` seam (empty when there is no import scope). Extension
    // candidates are BORROWED from the `Rc`-shared namespace records (kept alive in `ext_records`) —
    // deep-cloning every overload's `FunctionInfo` (params, call-sig vecs, generic sig) per call site
    // only to discard all but the winner dominated selection; only the selected overload is cloned.
    let owned_member_set;
    let owned_ext_records;
    let overloads: Vec<&FunctionInfo> = if let Some(functions) = provided_functions {
        functions.iter().collect()
    } else {
        match (mode, ext.fn_scope) {
            (SelectionMode::Kind(FnKind::Member), _) => {
                owned_member_set = members_in_hierarchy(src, recv, name).into_parts().0;
                owned_member_set.overloads.iter().collect()
            }
            (SelectionMode::Kind(FnKind::Extension), Some(scope)) => {
                owned_ext_records = symbols_in_function_scope(src, name, scope);
                owned_ext_records
                    .iter()
                    .flat_map(|record| match &record.callables {
                        crate::libraries::Callables::Functions(functions) => {
                            functions.overloads.as_slice()
                        }
                        crate::libraries::Callables::Both { functions, .. } => {
                            functions.overloads.as_slice()
                        }
                        _ => &[],
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    };
    // Candidates from the scoped query are IN-SCOPE by construction: each came from a `resolve_symbols`
    // over an imported package, so its declared package is in scope even when `@JvmPackageName` relocated
    // its facade to a different JVM package (`kotlin.collections`'s `UArraysKt` → `kotlin/collections/
    // unsigned/`). Re-deriving scope from the JVM owner (`fn_in_scope`) would wrongly drop those, so trust
    // the query.
    let pre_scoped = ext.fn_scope.is_some();
    crate::trace_compiler!(
        "resolve",
        "select_overload name={name} recv={recv:?} mode={mode:?} scope={:?} cands={}",
        ext.fn_scope.map(FunctionScopeRef::package_count),
        overloads.len(),
    );
    for o in &overloads {
        crate::trace_compiler!(
            "resolve",
            "  raw {name} kind={:?} recv={:?} params={:?} generic={:?} context={} required={} vararg={:?} pub={} rank={} origin={:?} owner={}",
            o.kind,
            o.semantic_receiver(),
            o.callable.params,
            o.generic_sig,
            o.context_count,
            o.call_sig.required,
            o.call_sig.vararg_index,
            o.public(),
            o.receiver_rank,
            o.callable.origin,
            o.callable.owner.render(),
        );
    }
    // Declaration-priority tiers, tried in this order and falling through whenever the tier holds no
    // applicable candidate (the `by_rank` walk below). Kotlin resolves a `@kotlin.internal.HidesMembers`
    // extension above members — the annotation exists so `Iterable<T>.forEach` wins over
    // `java.lang.Iterable.forEach(Consumer)` — and, as measured against kotlinc 2.4.10, above every
    // ordinary extension level as well, whatever its receiver specificity or lexical distance. Members
    // still precede ordinary extensions.
    const HIDES_MEMBERS_PRIORITY: u8 = 0;
    const MEMBER_PRIORITY: u8 = 1;
    const EXTENSION_PRIORITY: u8 = 2;

    let mut by_rank: std::collections::BTreeMap<(u8, u32, u32), Vec<(&FunctionInfo, Vec<Ty>)>> =
        std::collections::BTreeMap::new();
    let mut ranked: Vec<(u8, u32, u32, Ty, &FunctionInfo)> = Vec::new();
    if matches!(
        mode,
        SelectionMode::Kind(FnKind::Extension) | SelectionMode::Receiver
    ) {
        ranked.extend(
            ranked_extension_candidates(
                src,
                recv,
                ext.type_variables,
                overloads
                    .iter()
                    .copied()
                    .filter(|o| pre_scoped || fn_in_scope(o, ext.fn_scope)),
            )
            .into_iter()
            .map(|(rank, receiver, overload)| {
                (
                    if overload
                        .callable
                        .annotations
                        .contains(&crate::types::type_name("kotlin/internal/HidesMembers"))
                    {
                        HIDES_MEMBERS_PRIORITY
                    } else {
                        EXTENSION_PRIORITY
                    },
                    if overload
                        .callable
                        .annotations
                        .contains(&crate::types::type_name("kotlin/internal/HidesMembers"))
                    {
                        0
                    } else {
                        overload.scope_rank
                    },
                    rank,
                    receiver,
                    overload,
                )
            }),
        );
    }
    if matches!(
        mode,
        SelectionMode::Kind(FnKind::Member) | SelectionMode::Receiver
    ) && receiver_allows_member_dispatch(recv)
    {
        ranked.extend(
            overloads
                .iter()
                .copied()
                .filter(|o| o.kind == FnKind::Member)
                .map(|o| (MEMBER_PRIORITY, 0, o.receiver_rank, recv, o)),
        );
    }
    ranked.sort_by_key(|(priority, scope_rank, receiver_rank, _, _)| {
        (*priority, *scope_rank, *receiver_rank)
    });
    for (priority, scope_rank, receiver_rank, binding_receiver, o) in ranked {
        let lp = if indexed != IndexedConvention::Ordinary && o.call_sig.vararg_index.is_some() {
            let Some((params, _)) = indexed_call_shape(
                lib,
                src,
                o,
                binding_receiver,
                args,
                type_args,
                indexed == IndexedConvention::Set,
            ) else {
                crate::trace_compiler!(
                    "resolve",
                    "  drop {name} because indexed operands do not map to the declaration"
                );
                continue;
            };
            params
        } else {
            if !generic_function_call_admits(src, o, binding_receiver, args, type_args) {
                crate::trace_compiler!(
                    "resolve",
                    "  drop {name} because inferred type arguments violate declared bounds"
                );
                continue;
            }
            logical_call_params(src, o, binding_receiver, args, type_args)
        };
        let lp = apply_platform_call_parameter_nullability(
            lp,
            &o.call_sig.platform_nullable_params,
            &arg_tys,
            o.call_sig.vararg,
        );
        crate::trace_compiler!(
            "resolve",
            "  cand {name} scope_rank={scope_rank} receiver_rank={receiver_rank} logical_params={lp:?} owner={}",
            o.callable.owner.render()
        );
        by_rank
            .entry((priority, scope_rank, receiver_rank))
            .or_default()
            .push((o, lp));
    }
    if indexed != IndexedConvention::Ordinary {
        for cands in by_rank.values() {
            let fixed = cands
                .iter()
                .filter(|(candidate, _)| candidate.call_sig.vararg_index.is_none())
                .filter_map(|(candidate, params)| {
                    indexed_fixed_parameter_projection(candidate, params, args.len(), indexed)
                        .map(|projected| (*candidate, projected))
                })
                .collect::<Vec<_>>();
            match best_by_args(lib, assign_src, &fixed, args) {
                CandidateSelection::Selected(overload) => return Some(overload.clone()),
                CandidateSelection::Ambiguous => {
                    *ambiguous = true;
                    return None;
                }
                CandidateSelection::None => {}
            }
            let indexed_candidates = cands
                .iter()
                .filter_map(|(o, lp)| {
                    let vararg = o.call_sig.vararg_index?.checked_sub(o.context_count)?;
                    let element = lp.get(vararg)?.array_read_elem()?;
                    let trailing = usize::from(indexed == IndexedConvention::Set);
                    if vararg + 1 + trailing != lp.len() {
                        return None;
                    }
                    let index_count = args.len().checked_sub(trailing)?;
                    let mut expanded = lp[..vararg].to_vec();
                    expanded.extend(std::iter::repeat_n(
                        element,
                        index_count.checked_sub(vararg)?,
                    ));
                    if indexed == IndexedConvention::Set {
                        expanded.push(*lp.last()?);
                    }
                    Some((*o, expanded))
                })
                .collect::<Vec<_>>();
            match best_by_args(lib, assign_src, &indexed_candidates, args) {
                CandidateSelection::Selected(overload) => return Some(overload.clone()),
                CandidateSelection::Ambiguous => {
                    *ambiguous = true;
                    return None;
                }
                CandidateSelection::None => {}
            }
        }
        return None;
    }
    for cands in by_rank.values() {
        match best_by_args_with_ties(lib, assign_src, cands, args) {
            CandidateSelectionWithTies::Selected(overload) => return Some(overload.clone()),
            CandidateSelectionWithTies::Ambiguous(candidates) => {
                *ambiguous = true;
                if let Some(ambiguous_candidates) = &mut ambiguous_candidates {
                    ambiguous_candidates.extend(candidates.into_iter().cloned());
                }
                return None;
            }
            CandidateSelectionWithTies::None => {}
        }
    }
    // Vararg ELEMENT-expansion pass: a call passing loose elements (or nothing) where a
    // candidate declares a vararg (`"a.b".trim('.')` against `trim(vararg chars: Char)` — the
    // logical param is the ARRAY; `split('.')` against `split(vararg delimiters: Char,
    // ignoreCase: Boolean = false, limit: Int = 0)` — params after the vararg are reachable
    // only by name, so they must be defaulted). Two tiers per rank: EXACT element matches
    // first (`Char` argument selects the `Char` vararg over the `String` one, mirroring
    // most-specific selection), then platform/source-assignable elements.
    let vararg_shape = |o: &FunctionInfo, lp: &[Ty], exact: bool| -> Option<Vec<Ty>> {
        // A suspend callee's element-form vararg call would route the $default emission
        // outside the CPS pass's coverage — skip (unresolved), never ICE.
        if o.flags.suspend {
            return None;
        }
        let vararg_index = o.call_sig.vararg_index?;
        let array = lp.get(vararg_index).copied()?;
        let elem = array.array_read_elem()?;
        let applicable = args.len() >= vararg_index
            && lp[..vararg_index].iter().zip(args).all(|(p, a)| {
                let ty = a.ty();
                fun_arg_matches(assign_src, p, &ty, a.is_lambda_literal())
                    || semantic_arg_assignable(assign_src, p, &ty)
                    || a.binds_result_to(assign_src, *p)
            })
            && args[vararg_index..].iter().all(|a| {
                let ty = a.ty();
                // A SPREAD argument (`*xs`) fits the vararg's ARRAY type; a plain one, the element.
                let expected = if a.is_spread() { array } else { elem };
                ty == expected
                    || (!exact
                        && (semantic_arg_assignable(assign_src, &expected, &ty)
                            || a.binds_result_to(assign_src, expected)))
            })
            && (vararg_index + 1..lp.len()).all(|index| o.call_sig.param_has_default(index));
        applicable.then(|| {
            let mut shape = lp[..vararg_index].to_vec();
            shape.extend(args[vararg_index..].iter().map(|argument| {
                if argument.is_spread() {
                    array
                } else {
                    elem
                }
            }));
            shape
        })
    };
    for exact in [true, false] {
        for cands in by_rank.values() {
            let applicable = cands
                .iter()
                .filter_map(|(candidate, parameters)| {
                    vararg_shape(candidate, parameters, exact).map(|shape| (*candidate, shape))
                })
                .collect::<Vec<_>>();
            match best_by_args_with_ties(lib, assign_src, &applicable, args) {
                CandidateSelectionWithTies::Selected(candidate) => return Some(candidate.clone()),
                CandidateSelectionWithTies::Ambiguous(candidates) => {
                    *ambiguous = true;
                    if let Some(ambiguous_candidates) = &mut ambiguous_candidates {
                        ambiguous_candidates.extend(candidates.into_iter().cloned());
                    }
                    return None;
                }
                CandidateSelectionWithTies::None => {}
            }
        }
    }
    None
}

pub(super) fn generic_bounds_admit(
    src: &dyn SymbolSource,
    generic_sig: Option<&GenericSig>,
    receiver: Ty,
    args: &[Ty],
    type_args: &[Ty],
) -> bool {
    let Some(gsig) = generic_sig else {
        return true;
    };
    let mut binds = seeded_gsig_binds(gsig, type_args);
    let mut inferred = GSigBinds::new();
    if let Some(declared_receiver) = gsig.receiver {
        unify_inferred_ty_impl(Some(src), declared_receiver, receiver, &mut inferred);
    }
    for (&parameter, &argument) in gsig.params.iter().zip(args) {
        unify_inferred_ty_impl(Some(src), parameter, argument, &mut inferred);
    }
    merge_generic_bindings_from(Some(src), gsig, type_args, &mut binds, inferred);
    generic_bindings_satisfy_bounds(gsig, &binds, |actual, bound| {
        actual == bound
            || crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &SourceOracle(src),
                actual,
                bound,
            )
    })
}

/// Validate a function candidate's complete generic inference policy. Bounds and
/// `@OnlyInputTypes` are both declaration-owned constraints and must be decided before overload
/// ranking; otherwise a synthesized common supertype can make an inapplicable member/extension win
/// and the checker discovers the mismatch only after committing the wrong target.
fn generic_function_call_admits(
    src: &dyn SymbolSource,
    overload: &FunctionInfo,
    receiver: Ty,
    arguments: &[CallArgKind],
    type_args: &[Ty],
) -> bool {
    let signature = overload.semantic_signature();
    let mut bindings = seeded_gsig_binds(&signature, type_args);
    if let Some(declared_receiver) = signature.receiver {
        unify_ty_from_symbols(src, declared_receiver, receiver, &mut bindings);
    }
    let receiver_bindings = bindings.clone();
    let value_start = overload.context_count.min(signature.params.len());
    let actuals = signature.params[value_start..]
        .iter()
        .zip(arguments)
        .enumerate()
        .filter_map(|(value_parameter, (&declared, argument))| {
            let parameter = value_start + value_parameter;
            if !overload
                .call_sig
                .parameter_contributes_to_inference(parameter)
            {
                return None;
            }
            if !argument.contributes_type_to_inference() {
                return None;
            }
            Some((
                parameter,
                argument.inference_type(src, declared),
                argument.is_spread(),
            ))
        })
        .collect::<Vec<_>>();
    let only_input_actuals = signature.params[value_start..]
        .iter()
        .zip(arguments)
        .enumerate()
        .filter_map(|(value_parameter, (_, argument))| {
            let parameter = value_start + value_parameter;
            overload
                .call_sig
                .parameter_contributes_to_inference(parameter)
                .then(|| (parameter, argument.clone(), argument.is_spread()))
        })
        .collect::<Vec<_>>();
    let inferred = infer_generic_call_bindings_from_symbols(
        src,
        &signature,
        actuals.iter().copied(),
        overload.call_sig.vararg_index,
    );
    merge_call_argument_bindings(
        src,
        &signature,
        type_args,
        &receiver_bindings,
        &mut bindings,
        inferred,
    );
    apply_only_input_type_bindings(
        src,
        &signature,
        &overload.call_sig.only_input_type_formals,
        type_args,
        Some(receiver),
        &only_input_actuals,
        overload.call_sig.vararg_index,
        None,
        &mut bindings,
    ) && generic_bindings_satisfy_bounds(&signature, &bindings, |actual, bound| {
        resolution_subtype(src, actual, bound)
    })
}

pub(super) fn generic_bounds_admit_slots(
    src: &dyn SymbolSource,
    generic_sig: Option<&GenericSig>,
    call_sig: &CallSig,
    receiver: Ty,
    slots: &[Option<Ty>],
    type_args: &[Ty],
) -> bool {
    let Some(gsig) = generic_sig else {
        return true;
    };
    let mut binds = seeded_gsig_binds(gsig, type_args);
    let mut inferred = GSigBinds::new();
    if let Some(declared_receiver) = gsig.receiver {
        unify_inferred_ty_impl(Some(src), declared_receiver, receiver, &mut inferred);
    }
    for (index, (&parameter, argument)) in gsig.params.iter().zip(slots).enumerate() {
        if !call_sig.parameter_contributes_to_inference(index) {
            continue;
        }
        if let Some(argument) = argument {
            unify_inferred_ty_impl(Some(src), parameter, *argument, &mut inferred);
        }
    }
    merge_generic_bindings_from(Some(src), gsig, type_args, &mut binds, inferred);
    let actuals = slots
        .iter()
        .enumerate()
        .filter_map(|(parameter, argument)| {
            call_sig
                .parameter_contributes_to_inference(parameter)
                .then(|| argument.map(|argument| (parameter, CallArgKind::Typed(argument), false)))
                .flatten()
        })
        .collect::<Vec<_>>();
    apply_only_input_type_bindings(
        src,
        gsig,
        &call_sig.only_input_type_formals,
        type_args,
        Some(receiver),
        &actuals,
        call_sig.vararg_index,
        None,
        &mut binds,
    ) && generic_bindings_satisfy_bounds(gsig, &binds, |actual, bound| {
        actual == bound
            || crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &SourceOracle(src),
                actual,
                bound,
            )
    })
}

/// LOGICAL value parameters of an overload — what a call site's arguments are matched against, with the
/// receiver excluded (it is an attribute). Member/top-level `callable.params` are already value-only; an
/// extension's `callable.params` prepend the receiver in the JVM emit shape, so bind the generic signature
/// to `recv` and drop the leading receiver, preferring each parameter's value-class LOGICAL type over its
/// erased underlying (`Id` over `kotlin/String`).
pub(super) fn logical_value_params(
    source: &dyn SymbolSource,
    o: &FunctionInfo,
    recv: Ty,
    type_args: &[Ty],
) -> Vec<Ty> {
    let semantic = o.semantic_signature();
    let mut binds = seeded_gsig_binds(&semantic, type_args);
    if let Some(recv_sig) = semantic.receiver {
        unify_ty(recv_sig, recv, &mut binds);
    }
    let params = semantic
        .params
        .iter()
        .map(|parameter| {
            instantiate_slot(
                source,
                Some(&semantic),
                *parameter,
                &binds,
                TypePosition::In,
                UnboundSpecialization::Preserve,
            )
        })
        .collect::<Vec<_>>();
    params[o.context_count.min(params.len())..].to_vec()
}

/// Logical value parameters specialized by the complete call constraint set. Extension receivers and
/// arguments constrain the same declaration formals, so shaping them in separate passes can freeze a
/// bottom receiver result (`() -> Nothing`) before a concrete lambda supplies `String`.
pub(super) fn logical_call_params(
    source: &dyn SymbolSource,
    overload: &FunctionInfo,
    receiver: Ty,
    arguments: &[CallArgKind],
    type_arguments: &[Ty],
) -> Vec<Ty> {
    let signature = overload.semantic_signature();
    let mut bindings = seeded_gsig_binds(&signature, type_arguments);
    if let Some(declared_receiver) = signature.receiver {
        // The receiver is matched through its supertypes: a `List<E>` receiver binds the `T` of
        // `Collection<T>.plus`. A structural-only match leaves `T` open, so a collection argument
        // alone would fix it and `plus(element: T)` would out-rank `plus(elements: Iterable<T>)`.
        unify_ty_from_symbols(source, declared_receiver, receiver, &mut bindings);
    }
    // A receiver occurrence fixes the callable formal before value arguments are considered:
    // `String : Comparable<String>` makes the `T` in
    // `fun <T> Comparable<T>.compareTo(other: T)` exactly `String`. Joining a later `Int`
    // argument into that binding as the common supertype `Any` would rewrite the receiver to
    // `Comparable<Any>` and admit a call the receiver does not implement. Bottom is the sole open
    // receiver constraint: `() -> Nothing` may still be refined by a concrete value argument.
    let receiver_bindings = bindings.clone();
    let inferred = infer_generic_call_bindings_from_symbols(
        source,
        &signature,
        signature
            .params
            .iter()
            .zip(arguments)
            .enumerate()
            .filter_map(|(parameter, (&declared, argument))| {
                // A result-only nested call is not an input constraint; argument checking
                // propagates the selected parameter into it. A nested call whose own inputs
                // already fixed the result (`listOf(value)`, `xs.map { B(it) }`) is evidence:
                // `Collection<T>.plus` joins that element type with the receiver instead of
                // pinning `T` to the receiver alone.
                argument.supplies_fixed_argument_type().then_some((
                    parameter,
                    argument.inference_type(source, declared),
                    argument.is_spread(),
                ))
            }),
        overload.call_sig.vararg_index,
    );
    merge_call_argument_bindings(
        source,
        &signature,
        type_arguments,
        &receiver_bindings,
        &mut bindings,
        inferred,
    );
    let parameters = signature
        .params
        .iter()
        .map(|parameter| {
            instantiate_slot(
                source,
                Some(&signature),
                *parameter,
                &bindings,
                TypePosition::In,
                UnboundSpecialization::Preserve,
            )
        })
        .collect::<Vec<_>>();
    parameters[overload.context_count.min(parameters.len())..].to_vec()
}

/// Specialize a declaration for Kotlin's indexed-set operand convention. The final written operand
/// binds the final value parameter after all loose index operands bind the preceding vararg element.
pub(super) fn indexed_call_shape(
    lib: &dyn SemanticPlatform,
    source: &dyn SymbolSource,
    overload: &FunctionInfo,
    receiver: Ty,
    arguments: &[CallArgKind],
    type_arguments: &[Ty],
    set: bool,
) -> Option<(Vec<Ty>, Ty)> {
    let signature = overload.semantic_signature();
    let value_start = overload.context_count.min(signature.params.len());
    let vararg = overload.call_sig.vararg_index?;
    let logical_vararg = vararg.checked_sub(value_start)?;
    // Defaults between the vararg and value are valid Kotlin, but synthetic operator-call lowering
    // cannot realize omitted default slots yet. Keep selection aligned with that handoff.
    let trailing = usize::from(set);
    if vararg + 1 + trailing != signature.params.len() {
        return None;
    }
    let index_arguments = if set {
        arguments.split_last()?.1
    } else {
        arguments
    };
    if index_arguments.len() < logical_vararg {
        return None;
    }

    let mut bindings = seeded_gsig_binds(&signature, type_arguments);
    if let Some(declared_receiver) = signature.receiver {
        unify_ty_from_symbols(source, declared_receiver, receiver, &mut bindings);
    }
    let receiver_bindings = bindings.clone();
    let value_parameter = signature.params.len() - 1;
    let actuals = arguments
        .iter()
        .enumerate()
        .filter_map(|(source_index, argument)| {
            if !argument.supplies_fixed_argument_type() {
                return None;
            }
            let parameter = if source_index < logical_vararg {
                value_start + source_index
            } else if set && source_index + 1 == arguments.len() {
                value_parameter
            } else {
                vararg
            };
            let declared = *signature.params.get(parameter)?;
            let whole_array = argument.is_spread();
            let expected = if parameter == vararg && !whole_array {
                declared.array_read_elem().unwrap_or(declared)
            } else {
                declared
            };
            Some((
                parameter,
                argument.inference_type(source, expected),
                whole_array,
            ))
        });
    let inferred =
        infer_generic_call_bindings_from_symbols(source, &signature, actuals, Some(vararg));
    merge_call_argument_bindings(
        source,
        &signature,
        type_arguments,
        &receiver_bindings,
        &mut bindings,
        inferred,
    );
    if !generic_bindings_satisfy_bounds(&signature, &bindings, |actual, bound| {
        resolution_subtype(source, actual, bound)
    }) {
        return None;
    }
    let params = signature.params[value_start..]
        .iter()
        .map(|parameter| ty_subst_keep_unbound(*parameter, &bindings))
        .collect::<Vec<_>>();
    let inferred_ret = if overload.is_extension() {
        specialize_signature_output_type(source, signature.ret, &bindings)
    } else {
        ty_subst_keep_unbound(signature.ret, &bindings)
    };
    let ret = if overload.is_extension() {
        specialized_extension_return(lib, overload, inferred_ret)
    } else {
        overload
            .ret
            .apply(signature.apply_return_policy(lib, inferred_ret))
    };
    Some((params, ret))
}

/// Assignability through the SOURCE symbol federation (module classes first): a module-declared
/// class passed where a library member expects its (library) supertype — `class V : Thread()` into
/// `take(Thread)` — is invisible to the platform oracle, which only walks classpath supertypes.
pub(super) fn semantic_arg_assignable(src: &dyn SymbolSource, param: &Ty, arg: &Ty) -> bool {
    // Preserve a lexical type parameter's identity before consulting its upper bound. In
    // particular, `T?` is always assignable to the same `T?`; reducing the target to `Any?` first
    // loses that proof for nullable value-class applications inside a generic SAM result.
    if param == arg {
        return true;
    }
    if let Ty::TyParam(_, bound) = param.non_null() {
        if *arg == Ty::Null {
            return param.admits_null();
        }
        let expected = if param.is_nullable() {
            Ty::nullable(*bound)
        } else {
            *bound
        };
        return crate::assignable::is_assignable(
            &crate::assignable::TyCtx::new(),
            &SourceOracle(src),
            *arg,
            expected,
        );
    }
    crate::assignable::is_assignable(
        &crate::assignable::TyCtx::new(),
        &SourceOracle(src),
        *arg,
        *param,
    )
}

pub(super) fn distinct_source_declarations(left: &FunctionInfo, right: &FunctionInfo) -> bool {
    match (left.stable_declaration, right.stable_declaration) {
        (Some(left), Some(right)) => left != right,
        _ => {
            left.source_key.is_some()
                && right.source_key.is_some()
                && left.source_key != right.source_key
        }
    }
}

fn source_aware_most_specific_with_ties<'a, I>(
    candidates: I,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> CandidateSelectionWithTies<&'a FunctionInfo>
where
    I: Iterator<Item = (Vec<Ty>, &'a FunctionInfo)> + Clone,
{
    unique_most_specific_with_conflicts_and_ties(candidates, at_least_as_specific, |left, right| {
        distinct_source_declarations(left, right)
    })
}

fn declaration_specificity_params(candidate: &FunctionInfo) -> Vec<Ty> {
    let signature = candidate.semantic_signature();
    let mut params = signature
        .params
        .iter()
        .skip(candidate.context_count.min(signature.params.len()))
        .map(|parameter| ty_subst(*parameter, &GSigBinds::new()))
        .collect::<Vec<_>>();
    let vararg = candidate
        .call_sig
        .vararg_index
        .and_then(|index| index.checked_sub(candidate.context_count));
    if let Some(index) = vararg {
        if let Some(element) = params.get(index).and_then(|array| array.array_read_elem()) {
            params[index] = element;
        }
    }
    params
}

/// Pick the best overload whose logical value parameters accept `args`, in Kotlin applicability order:
/// exact, then widened or arity fits, then an omitted-default prefix, then a trailing lambda.
pub(crate) fn best_by_args<'a>(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    cands: &[(&'a FunctionInfo, Vec<Ty>)],
    args: &[CallArgKind],
) -> CandidateSelection<&'a FunctionInfo> {
    best_by_args_with_ties(lib, src, cands, args).collapse()
}

fn best_by_args_with_ties<'a>(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    cands: &[(&'a FunctionInfo, Vec<Ty>)],
    args: &[CallArgKind],
) -> CandidateSelectionWithTies<&'a FunctionInfo> {
    let ordinary = cands
        .iter()
        .filter(|(candidate, _)| {
            !candidate
                .callable
                .annotations
                .contains(&crate::types::type_name(
                    "kotlin/internal/LowPriorityInOverloadResolution",
                ))
        })
        .cloned()
        .collect::<Vec<_>>();
    match best_by_args_at_priority_with_ties(lib, src, &ordinary, args) {
        CandidateSelectionWithTies::None => {
            let low = cands
                .iter()
                .filter(|(candidate, _)| {
                    candidate
                        .callable
                        .annotations
                        .contains(&crate::types::type_name(
                            "kotlin/internal/LowPriorityInOverloadResolution",
                        ))
                })
                .cloned()
                .collect::<Vec<_>>();
            best_by_args_at_priority_with_ties(lib, src, &low, args)
        }
        selected => selected,
    }
}

fn best_by_args_at_priority_with_ties<'a>(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    cands: &[(&'a FunctionInfo, Vec<Ty>)],
    args: &[CallArgKind],
) -> CandidateSelectionWithTies<&'a FunctionInfo> {
    // Exact passes see runtime types; literal provenance only drives the adaptation passes.
    let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
    let adapts = |p: &Ty, arg: &CallArgKind, _i: usize| arg.adapts_integer_literal_to(*p);
    let function_like_fits = |p: &Ty, arg: &CallArgKind| {
        arg.function_type()
            .filter(|function| *function != arg.ty())
            .is_some_and(|function| {
                arg_fits_platform(lib, p, &function) || semantic_arg_assignable(src, p, &function)
            })
    };
    let fits = |_position: usize, p: &Ty, arg: &CallArgKind| {
        if arg.is_omitted_default() {
            return true;
        }
        let sam = p.non_null().fun_arity().is_none()
            && (arg.is_lambda_literal() || arg.function_type().is_some())
            && sam_arg_matches(lib, src, *p, arg.function_type().unwrap_or(arg.ty()));
        if arg.is_lambda_literal() && arg.ty() == Ty::Error {
            return untyped_lambda_pertinent(lib, src, *p);
        }
        sam || fun_arg_matches(src, p, &arg.ty(), arg.is_lambda_literal())
            || semantic_arg_assignable(src, p, &arg.ty())
            || (arg.ty() == Ty::Null && matches!(p.non_null(), Ty::TyParam(..)))
            || function_like_fits(p, arg)
            || arg.binds_result_to(src, *p)
    };
    match select_recorded_exact(cands, args, |_, left, right| {
        parameter_at_least_as_specific(src, left, right, CallArgKind::Typed(Ty::Error))
    }) {
        CandidateSelectionWithTies::Selected(candidate) => {
            return CandidateSelectionWithTies::Selected(candidate);
        }
        CandidateSelectionWithTies::Ambiguous(candidates)
            if candidates
                .iter()
                .all(|candidate| candidate.call_sig.vararg_index.is_none()) =>
        {
            return CandidateSelectionWithTies::Ambiguous(candidates);
        }
        CandidateSelectionWithTies::Ambiguous(_) | CandidateSelectionWithTies::None => {}
    }
    match integer_literal_overload_with_ties(
        cands
            .iter()
            .map(|(candidate, params)| (params.clone(), *candidate)),
        args,
        |position, param, arg| fits(position, param, arg),
        |_position, left, right, arg| parameter_at_least_as_specific(src, left, right, arg),
        |left, right| distinct_source_declarations(left, right),
    ) {
        CandidateSelectionWithTies::Selected(candidate) => {
            return CandidateSelectionWithTies::Selected(candidate);
        }
        CandidateSelectionWithTies::Ambiguous(candidates) => {
            return non_vararg_among(cands, &candidates, src);
        }
        CandidateSelectionWithTies::None => {}
    }
    let specificity = |_: usize, left: Ty, right: Ty| {
        parameter_at_least_as_specific(src, left, right, CallArgKind::Typed(Ty::Error))
    };

    // Contextual nested calls use this same comparison. Keeping them in a fixed-only pre-pass
    // would discard an applicable vararg element shape before specificity can compare it.
    match select_fixed_or_more_specific_vararg(cands, args, fits, specificity) {
        CandidateSelectionWithTies::Selected(candidate) => {
            return CandidateSelectionWithTies::Selected(candidate);
        }
        CandidateSelectionWithTies::Ambiguous(candidates) => {
            return CandidateSelectionWithTies::Ambiguous(candidates);
        }
        CandidateSelectionWithTies::None => {}
    }

    match source_aware_most_specific_with_ties(
        cands.iter().filter_map(|(candidate, params)| {
            (candidate.call_sig.required == 0 || candidate.call_sig.required <= args.len())
                .then(|| {
                    omitted_parameter_shape(params, args, |position, param, arg| {
                        fits(position, param, arg) || adapts(param, arg, position)
                    })
                    .map(|shape| (shape, *candidate))
                })
                .flatten()
        }),
        specificity,
    ) {
        CandidateSelectionWithTies::Selected(candidate) => {
            return CandidateSelectionWithTies::Selected(candidate);
        }
        CandidateSelectionWithTies::Ambiguous(candidates) => {
            return CandidateSelectionWithTies::Ambiguous(candidates)
        }
        CandidateSelectionWithTies::None => {}
    }

    if matches!(args.last(), Some(arg) if arg.ty().fun_arity().is_some()) {
        match source_aware_most_specific_with_ties(
            cands.iter().filter_map(|(candidate, params)| {
                let last = params.len().checked_sub(1)?;
                let prefix = args.len().checked_sub(1)?;
                let lambda_fits = prefix <= last && fits(last, &params[last], args.last().unwrap());
                let defaults_fit = (prefix..last)
                    .all(|i| candidate.call_sig.param_has_default(i))
                    || candidate.call_sig.required <= prefix;
                let prefix_fits = params[..prefix.min(params.len())]
                    .iter()
                    .zip(&arg_tys[..prefix])
                    .enumerate()
                    .all(|(i, (param, _arg))| {
                        fits(i, param, &args[i]) || adapts(param, &args[i], i)
                    });
                crate::trace_compiler!(
                    "resolve",
                    "trailing lambda candidate={} prefix={prefix} last={last} lambda_fits={lambda_fits} defaults_fit={defaults_fit} prefix_fits={prefix_fits} expected={:?} actual={:?}",
                    candidate.callable.name,
                    params[last],
                    args.last(),
                );
                (prefix <= last && lambda_fits && defaults_fit && prefix_fits)
                    .then(|| (params.clone(), *candidate))
            }),
            specificity,
        ) {
            CandidateSelectionWithTies::Selected(candidate) => {
                return CandidateSelectionWithTies::Selected(candidate);
            }
            CandidateSelectionWithTies::Ambiguous(candidates) => {
                return CandidateSelectionWithTies::Ambiguous(candidates)
            }
            CandidateSelectionWithTies::None => {}
        }
    }

    // At the candidate's OWN vararg slot: a slot-mapped call (`split(",", ignoreCase = false)`
    // against `split(vararg String, Boolean, Int)`) keeps the element at that slot, and a
    // non-final vararg is not the last parameter.
    source_aware_most_specific_with_ties(
        cands.iter().filter_map(|(candidate, params)| {
            candidate.call_sig.vararg.then(|| {
                candidate_vararg_shape(candidate, params, args, |position, param, arg| {
                    fits(position, param, arg) || adapts(param, arg, position)
                })
                .map(|shape| (shape, *candidate))
            })?
        }),
        specificity,
    )
}

/// A lambda argument (`Ty::Fun`) matches a decoded function-typed parameter of the same arity.
/// Providers must expose callable shape from metadata; source resolution never derives it from a
/// runtime classifier's spelling.
fn fun_arg_matches(
    src: &dyn SymbolSource,
    param: &Ty,
    arg: &Ty,
    allow_unit_coercion: bool,
) -> bool {
    let Some(arg_arity) = arg.fun_arity() else {
        return false;
    };
    let param = match param {
        Ty::Nullable(inner) => **inner,
        _ => *param,
    };
    param.fun_arity().is_some_and(|pn| pn == arg_arity)
        && fun_return_compatible(src, param, *arg, allow_unit_coercion)
}

/// A function-typed argument fits a function-typed parameter's RETURN. A parameter `(T) -> R` with a
/// CONCRETE `R` (`sumOfInt`'s `(T) -> Int`) accepts ONLY a lambda whose body returns that `R` — this is
/// how a `@OverloadResolutionByLambdaReturnType` group (whose overloads share value params and differ only
/// in the selector's return) is resolved: the lambda's return is just another parameter of the check. A
/// type-variable / erased-`Any` parameter return (an ordinary generic HOF `(T) -> R`), or an unresolved
/// lambda body, stays permissive so normal HOFs keep matching.
pub(super) fn fun_return_compatible(
    src: &dyn SymbolSource,
    param: Ty,
    arg: Ty,
    allow_unit_coercion: bool,
) -> bool {
    let (Some(pr), Some(ar)) = (param.fun_ret(), arg.fun_ret()) else {
        return true;
    };
    if matches!(pr.non_null(), Ty::TyParam(_, _))
        || matches!(pr, Ty::Error)
        || pr
            .non_null()
            .obj_internal()
            .is_some_and(|n| n == crate::types::wk::any())
        || (allow_unit_coercion && pr == Ty::Unit)
    {
        return true;
    }
    if matches!(ar, Ty::Error | Ty::Nothing) {
        return true;
    }
    if pr.non_null() == ar.non_null() {
        return true;
    }
    // A CONCRETE REFERENCE return is covariant: a lambda whose body returns a SUBTYPE (`String`) fits a
    // `(T) -> CharSequence` transform parameter (`joinToString`). Primitive returns stay INVARIANT — the
    // `@OverloadResolutionByLambdaReturnType` families (`sumOf { Int } / { Double }`) differ only by their
    // exact primitive return and must not cross-match.
    if let (Some(p), Some(a)) = (
        pr.non_null().kotlin_class_internal(),
        ar.non_null().kotlin_class_internal(),
    ) {
        if pr.is_reference() && ar.is_reference() {
            let compatible = resolution_subtype(src, Ty::obj_name(a), Ty::obj_name(p));
            crate::trace_compiler!(
                "resolve",
                "function return covariance actual={} expected={} compatible={compatible} actual_supertypes={:?}",
                a.render(),
                p.render(),
                src.classifier(a)
                    .map(|classifier| classifier.supertypes.iter_rendered().collect::<Vec<_>>())
                    .unwrap_or_default(),
            );
            return compatible;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{
        select_fixed_or_more_specific_vararg, CandidateSelectionWithTies, ReceiverFunctionSelection,
    };
    use crate::libraries::{CallSig, FnKind, FunctionInfo, LibraryCallable};
    use crate::types::Ty;

    fn candidate(name: &str, parameter: Ty, vararg: bool) -> FunctionInfo {
        let callable = LibraryCallable::library(
            "fixture/OverloadsKt",
            name,
            vec![parameter],
            Ty::String,
            Ty::String,
            "(Ljava/lang/Object;)Ljava/lang/String;",
        );
        let mut candidate = FunctionInfo::plain(FnKind::TopLevel, None, callable);
        if vararg {
            candidate.call_sig = CallSig {
                required: 1,
                vararg: true,
                vararg_index: Some(0),
                ..Default::default()
            };
        }
        candidate
    }

    #[test]
    fn a_receiver_selection_carries_a_pointer_to_its_callable() {
        assert_eq!(std::mem::size_of::<ReceiverFunctionSelection>(), 96);
    }

    #[test]
    fn contextual_vararg_uses_its_element_before_its_raw_array() {
        let root = Ty::obj("fixture/Root");
        let leaf = Ty::obj("fixture/Leaf");
        let fixed = candidate("pick", root, false);
        let vararg = candidate("pick", Ty::array(leaf), true);
        let candidates = [(&fixed, vec![root]), (&vararg, vec![Ty::array(leaf)])];
        let args = [super::CallArgKind::Typed(Ty::Error)];

        let selected = select_fixed_or_more_specific_vararg(
            &candidates,
            &args,
            |_, _, _| true,
            |_, left, right| left == right || (left == leaf && right == root),
        );

        assert!(matches!(
            selected,
            CandidateSelectionWithTies::Selected(candidate)
                if std::ptr::eq(candidate, &vararg)
        ));
    }
}
