//! Provider-neutral callable-reference specialization shared by body checking and compact
//! signature evaluation.

use crate::libraries::{CallSig, FunctionInfo};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdaptedRefArgument {
    Value(usize),
    Default,
    Vararg {
        values: Vec<usize>,
        whole_array: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CallableRefSpecialization {
    Specialized,
    UnresolvedTypeArguments,
    Inapplicable,
}

pub(super) struct SpecializedGenericCallableReference {
    pub(super) receiver: Option<Ty>,
    pub(super) params: Vec<Ty>,
    pub(super) ret: Ty,
    /// Final arguments in declaration-formal order. Selection owns inference; checked FIR merely
    /// publishes these bindings and lowering consumes them without reconstructing specialization
    /// from the adapted function shape.
    pub(super) type_arguments: Vec<Ty>,
}

fn alpha_equivalent_type(
    left: Ty,
    right: Ty,
    formals: &std::collections::HashMap<String, String>,
) -> bool {
    if left == right {
        return true;
    }
    match (left, right) {
        (Ty::TyParam(left, _), Ty::TyParam(right, _)) => {
            formals.get(left).is_some_and(|mapped| mapped == right)
        }
        (Ty::Nullable(left), Ty::Nullable(right))
        | (Ty::PlatformNullable(left), Ty::PlatformNullable(right))
        | (Ty::InProjection(left), Ty::InProjection(right))
        | (Ty::OutProjection(left), Ty::OutProjection(right))
        | (Ty::StarProjection(left), Ty::StarProjection(right)) => {
            alpha_equivalent_type(*left, *right, formals)
        }
        (Ty::Obj(left, left_arguments), Ty::Obj(right, right_arguments)) => {
            left == right
                && left_arguments.len() == right_arguments.len()
                && left_arguments
                    .iter()
                    .zip(right_arguments)
                    .all(|(&left, &right)| alpha_equivalent_type(left, right, formals))
        }
        (Ty::Fun(left), Ty::Fun(right)) => {
            left.params.len() == right.params.len()
                && left.context_count == right.context_count
                && left.has_receiver == right.has_receiver
                && left.suspend == right.suspend
                && left
                    .params
                    .iter()
                    .zip(&right.params)
                    .all(|(&left, &right)| alpha_equivalent_type(left, right, formals))
                && alpha_equivalent_type(left.ret, right.ret, formals)
        }
        _ => false,
    }
}

/// Method formals bound from a projected classifier receiver denote the receiver's EXISTENTIAL
/// capture, not its readable upper bound. The capture is already valid against the classifier's
/// declaration. It may satisfy the method bound without the upper-bound approximation satisfying
/// that bound as a concrete type (`C : Comparable<C>` reads as `Comparable<C>` through `A3<*>`, but
/// `Comparable<C>` is not itself a `Comparable<Comparable<C>>`). This exemption is sound only when
/// the method bound is the classifier bound modulo formal names; a stricter extension must still be
/// rejected.
fn receiver_validated_projected_formals(
    source: &dyn SymbolSource,
    generic: &crate::libraries::GenericSig,
    declared_receiver: Ty,
    actual_receiver: Ty,
) -> std::collections::HashSet<String> {
    let (Ty::Obj(declared_owner, declared_arguments), Ty::Obj(actual_owner, actual_arguments)) =
        (declared_receiver.non_null(), actual_receiver.non_null())
    else {
        return std::collections::HashSet::new();
    };
    if declared_owner != actual_owner || declared_arguments.len() != actual_arguments.len() {
        return std::collections::HashSet::new();
    }
    let Some(classifier) = source.classifier(declared_owner) else {
        return std::collections::HashSet::new();
    };
    if classifier.type_params.len() != declared_arguments.len() {
        return std::collections::HashSet::new();
    }
    let mut alpha = std::collections::HashMap::new();
    for (&declared, classifier_formal) in declared_arguments.iter().zip(&classifier.type_params) {
        let declared = declared.projection_inner().unwrap_or(declared);
        if let Ty::TyParam(method_formal, _) = declared {
            if generic.formals.iter().any(|formal| formal == method_formal) {
                alpha.insert(method_formal.to_string(), classifier_formal.clone());
            }
        }
    }
    let mut validated = std::collections::HashSet::new();
    for (index, (&declared, &actual)) in declared_arguments.iter().zip(actual_arguments).enumerate()
    {
        if actual.projection_inner().is_none() {
            continue;
        }
        let declared = declared.projection_inner().unwrap_or(declared);
        let Ty::TyParam(method_formal, _) = declared else {
            continue;
        };
        let Some(method_index) = generic
            .formals
            .iter()
            .position(|formal| formal == method_formal)
        else {
            continue;
        };
        let method_bounds = generic
            .formal_bounds
            .get(method_index)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let classifier_bounds = classifier
            .type_param_bounds
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or_default();
        // A classifier record may include additional normalized/implied upper bounds. The method
        // needs no stronger proof than the capture already has, so every method bound must occur in
        // the classifier's bound set; the sets need not have identical cardinality.
        let equivalent = method_bounds.iter().all(|&method| {
            classifier_bounds
                .iter()
                .any(|&classifier| alpha_equivalent_type(method, classifier, &alpha))
        });
        crate::trace_compiler!(
            "callable_ref",
            "projected receiver formal={method_formal:?} method_bounds={method_bounds:?} classifier_formal={:?} classifier_bounds={classifier_bounds:?} alpha={alpha:?} equivalent={equivalent}",
            classifier.type_params.get(index),
        );
        if equivalent {
            validated.insert(method_formal.to_string());
        }
    }
    validated
}

pub(super) fn specialize_generic_signature(
    source: &dyn SymbolSource,
    generic: &crate::libraries::GenericSig,
    call_sig: &CallSig,
    extension_receiver: Option<Ty>,
    expected_params: Option<&[Ty]>,
    expected_ret: Option<Ty>,
    mut is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> Result<SpecializedGenericCallableReference, CallableRefSpecialization> {
    let mut bindings = crate::symbol_resolver::GSigBinds::new();
    if let (Some(declared), Some(actual)) = (generic.receiver, extension_receiver) {
        let mut inferred = crate::symbol_resolver::GSigBinds::new();
        crate::symbol_resolver::unify_ty_from_symbols(source, declared, actual, &mut inferred);
        crate::symbol_resolver::merge_generic_bindings(generic, 0, &mut bindings, inferred);
    }
    if let Some(expected) = expected_params {
        let vararg = call_sig.vararg_index;
        let whole_vararg = vararg.is_some_and(|index| {
            expected.len() == generic.params.len()
                && expected
                    .get(index)
                    .is_some_and(|parameter| parameter.array_elem().is_some())
        });
        let actuals = expected
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, actual)| {
                let parameter = match vararg {
                    Some(vararg) if index >= vararg => vararg,
                    _ if index < generic.params.len() => index,
                    _ => return None,
                };
                Some((parameter, actual, whole_vararg && index == parameter))
            });
        let inferred = crate::symbol_resolver::infer_generic_call_bindings_from_symbols(
            source, generic, actuals, vararg,
        );
        crate::symbol_resolver::merge_generic_bindings_from(
            Some(source),
            generic,
            0,
            &mut bindings,
            inferred,
        );
    }
    if let Some(expected) = expected_ret {
        if let Some(inferred) = crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
            source,
            generic,
            expected,
            |actual, target| is_assignable(actual, target),
        ) {
            crate::symbol_resolver::merge_generic_upper_bindings(
                generic,
                0,
                &mut bindings,
                inferred,
                |actual, target| is_assignable(actual, target),
            );
        }
    }
    let receiver_validated = match (generic.receiver, extension_receiver) {
        (Some(declared), Some(actual)) => {
            receiver_validated_projected_formals(source, generic, declared, actual)
        }
        _ => std::collections::HashSet::new(),
    };
    let mut validation_bindings = bindings.clone();
    for formal in receiver_validated {
        validation_bindings.remove(&formal);
    }
    if !crate::symbol_resolver::generic_bindings_satisfy_bounds(
        generic,
        &validation_bindings,
        |actual, bound| is_assignable(actual, bound),
    ) {
        return Err(CallableRefSpecialization::Inapplicable);
    }
    if generic
        .formals
        .iter()
        .any(|formal| !bindings.contains_key(formal))
    {
        return Err(CallableRefSpecialization::UnresolvedTypeArguments);
    }
    let specialized_receiver = generic
        .receiver
        .map(|receiver| {
            crate::symbol_resolver::specialize_signature_receiver_type(source, receiver, &bindings)
        })
        .or(extension_receiver);
    let values = generic
        .params
        .iter()
        .map(|parameter| {
            crate::symbol_resolver::specialize_signature_input_type(source, *parameter, &bindings)
        })
        .collect::<Vec<_>>();
    Ok(SpecializedGenericCallableReference {
        receiver: specialized_receiver,
        params: values,
        ret: crate::symbol_resolver::specialize_signature_output_type(
            source,
            generic.ret,
            &bindings,
        ),
        type_arguments: generic
            .formals
            .iter()
            .map(|formal| bindings[formal])
            .collect(),
    })
}

pub(super) fn specialize_candidate_with_type_arguments(
    source: &dyn SymbolSource,
    function: &mut FunctionInfo,
    extension_receiver: Option<Ty>,
    expected_params: Option<&[Ty]>,
    expected_ret: Option<Ty>,
    is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> Result<Vec<Ty>, CallableRefSpecialization> {
    let Some(generic) = function.generic_sig.clone() else {
        return Ok(Vec::new());
    };
    let specialized = match specialize_generic_signature(
        source,
        &generic,
        &function.call_sig,
        extension_receiver,
        expected_params,
        expected_ret,
        is_assignable,
    ) {
        Ok(specialized) => specialized,
        Err(failure) => return Err(failure),
    };
    let type_arguments = specialized.type_arguments;
    function.receiver = specialized.receiver;
    function.callable.params = if function.is_extension() {
        specialized
            .receiver
            .into_iter()
            .chain(specialized.params)
            .collect()
    } else {
        specialized.params
    };
    function.callable.ret = specialized.ret;
    function.generic_sig = None;
    Ok(type_arguments)
}

pub(super) fn specialize_candidate(
    source: &dyn SymbolSource,
    function: &mut FunctionInfo,
    extension_receiver: Option<Ty>,
    expected_params: Option<&[Ty]>,
    expected_ret: Option<Ty>,
    is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> CallableRefSpecialization {
    specialize_candidate_with_type_arguments(
        source,
        function,
        extension_receiver,
        expected_params,
        expected_ret,
        is_assignable,
    )
    .map_or_else(
        |failure| failure,
        |_| CallableRefSpecialization::Specialized,
    )
}

pub(super) fn is_compatible(
    params: &[Ty],
    ret: Ty,
    suspend: bool,
    expected: &'static crate::types::FnSig,
    allow_unit_coercion: bool,
    mut is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> bool {
    if (suspend && !expected.suspend) || params.len() != expected.params.len() {
        return false;
    }
    let mut result_bindings = crate::symbol_resolver::GSigBinds::new();
    crate::symbol_resolver::unify_inferred_ty(expected.ret, ret, &mut result_bindings);
    let expected_ret =
        crate::symbol_resolver::ty_subst_keep_unbound(expected.ret, &result_bindings);
    expected
        .params
        .iter()
        .zip(params)
        .all(|(expected, actual)| {
            let expected =
                crate::symbol_resolver::ty_subst_keep_unbound(*expected, &result_bindings);
            match expected.non_null() {
                Ty::TyParam(_, bound) => is_assignable(*actual, *bound),
                _ => is_assignable(expected, *actual),
            }
        })
        && ((allow_unit_coercion && expected.ret == Ty::Unit) || is_assignable(ret, expected_ret))
}

/// Replace provisional type-variable slots in an expected callable shape with the concrete slots
/// selected from one declaration. The argument plan is the ownership boundary: it records which
/// adapter input supplies each declaration parameter, including whole-array and collected varargs.
pub(super) fn realize_expected_shape(
    expected: &'static crate::types::FnSig,
    target_params: &[Ty],
    target_ret: Ty,
    plan: &[AdaptedRefArgument],
) -> Ty {
    let mut bindings = crate::symbol_resolver::GSigBinds::new();
    for (target, argument) in plan.iter().enumerate() {
        let Some(target_parameter) = target_params.get(target).copied() else {
            continue;
        };
        match argument {
            AdaptedRefArgument::Value(value) => {
                if let Some(expected_parameter) = expected.params.get(*value).copied() {
                    crate::symbol_resolver::unify_inferred_ty(
                        expected_parameter,
                        target_parameter,
                        &mut bindings,
                    );
                }
            }
            AdaptedRefArgument::Vararg {
                values,
                whole_array,
            } => {
                let parameter = if *whole_array {
                    target_parameter
                } else {
                    target_parameter
                        .array_read_elem()
                        .unwrap_or(target_parameter)
                };
                for value in values {
                    if let Some(expected_parameter) = expected.params.get(*value).copied() {
                        crate::symbol_resolver::unify_inferred_ty(
                            expected_parameter,
                            parameter,
                            &mut bindings,
                        );
                    }
                }
            }
            AdaptedRefArgument::Default => {}
        }
    }
    crate::symbol_resolver::unify_inferred_ty(expected.ret, target_ret, &mut bindings);
    let parameters = expected
        .params
        .iter()
        .map(|parameter| crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &bindings))
        .collect();
    let result = crate::symbol_resolver::ty_subst_keep_unbound(expected.ret, &bindings);
    Ty::fun_with_shape(
        parameters,
        result,
        expected.context_count,
        expected.has_receiver,
        expected.suspend,
    )
}

/// Whether one contextual callable-input slot may be supplied by a declaration input. Function
/// inputs are contravariant: a concrete contextual input must be accepted by the declaration.
/// An unresolved contextual type parameter is an inference slot, however, so its upper bound is
/// checked here and the selected declaration's concrete input is published by
/// [`realize_expected_shape`].
pub(super) fn input_parameter_fits(
    expected: Ty,
    target: Ty,
    mut is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> bool {
    match (expected.non_null(), target.non_null()) {
        (Ty::TyParam(_, bound), _) => is_assignable(target, *bound),
        (_, Ty::TyParam(_, bound)) => is_assignable(expected, *bound),
        _ => is_assignable(expected, target),
    }
}

pub(super) fn parameter_plan(
    params: &[Ty],
    call_sig: &CallSig,
    expected_params: &[Ty],
    mut is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> Option<Vec<AdaptedRefArgument>> {
    fn whole_vararg_array_fits(
        actual: Ty,
        target: Ty,
        is_assignable: &mut impl FnMut(Ty, Ty) -> bool,
    ) -> bool {
        input_parameter_fits(actual, target, &mut *is_assignable)
            || (actual.is_reference_array()
                && target.is_reference_array()
                && actual
                    .array_read_elem()
                    .zip(target.array_read_elem())
                    .is_some_and(|(actual, target)| {
                        input_parameter_fits(actual, target, &mut *is_assignable)
                    }))
    }

    fn map_from(
        params: &[Ty],
        call_sig: &CallSig,
        expected_params: &[Ty],
        target: usize,
        value: usize,
        out: &mut Vec<AdaptedRefArgument>,
        is_assignable: &mut impl FnMut(Ty, Ty) -> bool,
    ) -> bool {
        if target == params.len() {
            return value == expected_params.len();
        }
        if call_sig.vararg_index == Some(target) {
            let array = params[target];
            let Some(element) = array.array_elem() else {
                return false;
            };
            if value < expected_params.len()
                && whole_vararg_array_fits(expected_params[value], array, is_assignable)
            {
                out.push(AdaptedRefArgument::Vararg {
                    values: vec![value],
                    whole_array: true,
                });
                if map_from(
                    params,
                    call_sig,
                    expected_params,
                    target + 1,
                    value + 1,
                    out,
                    is_assignable,
                ) {
                    return true;
                }
                out.pop();
            }
            let available = expected_params.len().saturating_sub(value);
            for count in (1..=available).rev() {
                if !expected_params[value..value + count]
                    .iter()
                    .all(|actual| input_parameter_fits(*actual, element, &mut *is_assignable))
                {
                    continue;
                }
                out.push(AdaptedRefArgument::Vararg {
                    values: (value..value + count).collect(),
                    whole_array: false,
                });
                if map_from(
                    params,
                    call_sig,
                    expected_params,
                    target + 1,
                    value + count,
                    out,
                    is_assignable,
                ) {
                    return true;
                }
                out.pop();
            }
            if call_sig.param_has_default(target) {
                out.push(AdaptedRefArgument::Default);
            } else {
                out.push(AdaptedRefArgument::Vararg {
                    values: Vec::new(),
                    whole_array: false,
                });
            }
            if map_from(
                params,
                call_sig,
                expected_params,
                target + 1,
                value,
                out,
                is_assignable,
            ) {
                return true;
            }
            out.pop();
            return false;
        }
        if value < expected_params.len()
            && input_parameter_fits(expected_params[value], params[target], &mut *is_assignable)
        {
            out.push(AdaptedRefArgument::Value(value));
            if map_from(
                params,
                call_sig,
                expected_params,
                target + 1,
                value + 1,
                out,
                is_assignable,
            ) {
                return true;
            }
            out.pop();
        }
        if call_sig.param_has_default(target) {
            out.push(AdaptedRefArgument::Default);
            if map_from(
                params,
                call_sig,
                expected_params,
                target + 1,
                value,
                out,
                is_assignable,
            ) {
                return true;
            }
            out.pop();
        }
        false
    }

    let mut plan = Vec::with_capacity(params.len());
    map_from(
        params,
        call_sig,
        expected_params,
        0,
        0,
        &mut plan,
        &mut is_assignable,
    )
    .then_some(plan)
}

/// Select an expected-shape instance reference from ordinary member candidates.
///
/// Body checking and compact-signature evaluation must make the same adaptation, generic
/// specialization, receiver-rank, cost, duplicate-fact, and specificity decision. Keeping that
/// decision here also lets signature evaluation try members before it demands any extension
/// candidate whose own inferred signature may currently be under computation.
fn select_adapted_bound_instance_candidate(
    source: &dyn SymbolSource,
    overloads: &[FunctionInfo],
    expected: &'static crate::types::FnSig,
    mut is_assignable: impl FnMut(Ty, Ty) -> bool,
    mut shape_at_least_as_specific: impl FnMut(&[Ty], Ty, &[Ty], Ty) -> bool,
) -> Option<(FunctionInfo, Vec<AdaptedRefArgument>, Vec<Ty>)> {
    let mut candidates = overloads
        .iter()
        .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
        .cloned()
        .filter_map(|mut candidate| {
            if candidate.callable.suspend && !expected.suspend {
                return None;
            }
            let type_arguments = specialize_candidate_with_type_arguments(
                source,
                &mut candidate,
                None,
                Some(&expected.params),
                Some(expected.ret),
                |actual, bound| is_assignable(actual, bound),
            )
            .ok()?;
            let plan = parameter_plan(
                &candidate.semantic_params(),
                &candidate.call_sig,
                &expected.params,
                |actual, target| is_assignable(actual, target),
            )?;
            if !is_compatible(
                &expected.params,
                candidate.callable.ret,
                candidate.callable.suspend,
                expected,
                true,
                |actual, target| is_assignable(actual, target),
            ) {
                return None;
            }
            let cost = plan_cost(&plan);
            Some((candidate, plan, cost, type_arguments))
        })
        .collect::<Vec<_>>();
    let nearest = candidates
        .iter()
        .map(|candidate| candidate.0.receiver_rank)
        .min()?;
    candidates.retain(|candidate| candidate.0.receiver_rank == nearest);
    let best = candidates.iter().map(|candidate| candidate.2).min()?;
    candidates.retain(|candidate| candidate.2 == best);

    // Providers may expose one inherited declaration through multiple hierarchy paths. Those are
    // duplicate facts; distinct semantic signatures remain overload candidates.
    let mut unique = Vec::<(FunctionInfo, Vec<AdaptedRefArgument>, usize, Vec<Ty>)>::new();
    for candidate in candidates {
        if unique.iter().any(|existing| {
            existing.0.semantic_params() == candidate.0.semantic_params()
                && existing.0.callable.ret == candidate.0.callable.ret
                && existing.0.callable.suspend == candidate.0.callable.suspend
        }) {
            continue;
        }
        unique.push(candidate);
    }
    let maximal = unique
        .iter()
        .enumerate()
        .filter_map(|(index, current)| {
            let dominated = unique.iter().enumerate().any(|(other_index, other)| {
                index != other_index
                    && shape_at_least_as_specific(
                        &other.0.semantic_params(),
                        other.0.callable.ret,
                        &current.0.semantic_params(),
                        current.0.callable.ret,
                    )
                    && !shape_at_least_as_specific(
                        &current.0.semantic_params(),
                        current.0.callable.ret,
                        &other.0.semantic_params(),
                        other.0.callable.ret,
                    )
            });
            (!dominated).then_some(index)
        })
        .collect::<Vec<_>>();
    let [selected] = maximal.as_slice() else {
        return None;
    };
    let (selected, plan, _, type_arguments) = unique.swap_remove(*selected);
    Some((selected, plan, type_arguments))
}

/// Select an adapted instance reference. `unbound_receiver` is present for
/// `Classifier::member`: its leading contextual input supplies the dispatch receiver, while the
/// returned argument plan owns only declaration value parameters. Bound and unbound callers share
/// this entry point so they cannot drift into different specialization or overload-selection rules.
pub(super) fn select_adapted_instance_candidate(
    source: &dyn SymbolSource,
    overloads: &[FunctionInfo],
    unbound_receiver: Option<Ty>,
    expected: &'static crate::types::FnSig,
    mut is_assignable: impl FnMut(Ty, Ty) -> bool,
    shape_at_least_as_specific: impl FnMut(&[Ty], Ty, &[Ty], Ty) -> bool,
) -> Option<(FunctionInfo, Vec<AdaptedRefArgument>, Vec<Ty>)> {
    let method = match unbound_receiver {
        Some(receiver) => {
            let (&expected_receiver, expected_values) = expected.params.split_first()?;
            if !input_parameter_fits(expected_receiver, receiver, &mut is_assignable) {
                return None;
            }
            let Ty::Fun(method) = Ty::fun_with_shape(
                expected_values.to_vec(),
                expected.ret,
                expected.context_count,
                false,
                expected.suspend,
            ) else {
                unreachable!("unbound member expectation is always a function")
            };
            method
        }
        None => expected,
    };
    let (selected, mut plan, type_arguments) = select_adapted_bound_instance_candidate(
        source,
        overloads,
        method,
        is_assignable,
        shape_at_least_as_specific,
    )?;
    if unbound_receiver.is_some() {
        shift_plan_values(&mut plan, 1);
    }
    Some((selected, plan, type_arguments))
}

/// Realize the semantic function shape selected above. The lowering plan intentionally excludes
/// the dispatch receiver; this helper adds that input only for contextual-shape inference and
/// returns the complete natural parameter list used by exact-reference detection.
pub(super) fn realize_adapted_instance_shape(
    expected: &'static crate::types::FnSig,
    unbound_receiver: Option<Ty>,
    target_params: &[Ty],
    target_ret: Ty,
    argument_mapping: &[AdaptedRefArgument],
) -> (Ty, Vec<Ty>) {
    let mut natural_params = target_params.to_vec();
    let shape_plan = match unbound_receiver {
        Some(receiver) => {
            natural_params.insert(0, receiver);
            std::iter::once(AdaptedRefArgument::Value(0))
                .chain(argument_mapping.iter().cloned())
                .collect::<Vec<_>>()
        }
        None => argument_mapping.to_vec(),
    };
    (
        realize_expected_shape(expected, &natural_params, target_ret, &shape_plan),
        natural_params,
    )
}

pub(super) fn plan_cost(plan: &[AdaptedRefArgument]) -> usize {
    plan.iter()
        .map(|argument| match argument {
            AdaptedRefArgument::Value(_) => 0,
            AdaptedRefArgument::Vararg {
                whole_array: true, ..
            } => 1,
            AdaptedRefArgument::Vararg { .. } => 10,
            AdaptedRefArgument::Default => 100,
        })
        .sum()
}

pub(super) fn plan_is_identity_from(plan: &[AdaptedRefArgument], value_offset: usize) -> bool {
    plan.iter().enumerate().all(|(target, argument)| {
        matches!(argument, AdaptedRefArgument::Value(value) if *value == target + value_offset)
    })
}

pub(super) fn shift_plan_values(plan: &mut [AdaptedRefArgument], offset: usize) {
    shift_plan_values_from(plan, 0, offset);
}

pub(super) fn shift_plan_values_from(
    plan: &mut [AdaptedRefArgument],
    first_value: usize,
    offset: usize,
) {
    for argument in plan {
        match argument {
            AdaptedRefArgument::Value(value) if *value >= first_value => *value += offset,
            AdaptedRefArgument::Vararg { values, .. } => {
                values
                    .iter_mut()
                    .filter(|value| **value >= first_value)
                    .for_each(|value| *value += offset);
            }
            AdaptedRefArgument::Value(_) | AdaptedRefArgument::Default => {}
        }
    }
}
