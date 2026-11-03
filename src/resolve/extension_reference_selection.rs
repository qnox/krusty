//! Selecting an extension callable reference from its overload family.
//!
//! Checked bodies and compact signature evaluation select the same way. Each extension is
//! specialized against the reference receiver and the expected function shape, its inputs are
//! mapped onto that shape, and the most specific applicable declarations at the nearest receiver
//! rank win. The two callers differ only in their subtype oracle, accessibility, and the evidence a
//! checked body holds for a still-postponed outer call.

use super::callable_reference_selection::{
    maximal_adapted_references, parameter_plan, shift_plan_values_from,
    specialize_candidate_with_type_arguments, AdaptedRefArgument, AdaptedReferenceSpecificity,
};
use crate::libraries::{CallSig, FunctionInfo};
use crate::symbol_source::SymbolSource;
use crate::types::{FnSig, Ty};

/// The caller-owned facts one extension-reference selection consults.
pub(super) trait ExtensionReferenceRules {
    /// Whether `actual` satisfies a declared type-parameter `bound`.
    fn bound_admits(&self, actual: Ty, bound: Ty) -> bool;
    /// Whether a value of type `actual` may be used where `target` is expected.
    fn assignable(&self, actual: Ty, target: Ty) -> bool;
    /// Whether this use site may see `function`.
    fn accessible(&self, function: &FunctionInfo) -> bool;
    /// Evidence beyond the expected shape that its `expected` input still admits `target`.
    fn input_fits(&self, _expected: Ty, _target: Ty) -> bool {
        true
    }
    /// Whether the `left` adapted parameter shape is at least as specific as `right`.
    fn at_least_as_specific(&self, left: &[Ty], right: &[Ty]) -> bool;
}

pub(super) enum ExtensionReferenceChoice {
    None,
    Selected(Box<(FunctionInfo, Vec<AdaptedRefArgument>, Vec<Ty>)>),
    Ambiguous(Vec<FunctionInfo>),
}

/// Select the extension a reference on `receiver` names. An `unbound` reference passes the
/// receiver as its first non-context input; a bound one has already applied it.
pub(super) fn select_extension_reference(
    source: &dyn SymbolSource,
    overloads: &[FunctionInfo],
    receiver: Ty,
    unbound: bool,
    expected: Option<&'static FnSig>,
    rules: &impl ExtensionReferenceRules,
) -> ExtensionReferenceChoice {
    let candidates = overloads
        .iter()
        .cloned()
        .filter_map(|function| {
            applicable_extension(source, function, receiver, unbound, expected, rules)
        })
        .collect::<Vec<_>>();
    let Some(nearest) = candidates
        .iter()
        .map(|candidate| candidate.function.receiver_rank)
        .min()
    else {
        return ExtensionReferenceChoice::None;
    };
    let candidates = candidates
        .into_iter()
        .filter(|candidate| candidate.function.receiver_rank == nearest)
        .collect::<Vec<_>>();
    // Overload specificity is a property of the DECLARATIONS. The expected/adapted function
    // shape is intentionally absent here: every compatible overload adapts to that same shape, so
    // comparing it would turn `C::pick` for `(C, Int) -> Unit` into a false tie between
    // `pick(Int)` and `pick(Any)`.
    let specificity = candidates
        .iter()
        .map(|candidate| AdaptedReferenceSpecificity {
            parameters: &candidate.parameters,
            plan: &candidate.plan,
            is_vararg: candidate.function.call_sig.vararg_index.is_some(),
        })
        .collect::<Vec<_>>();
    let maximal = maximal_adapted_references(&specificity, |left, right| {
        rules.at_least_as_specific(left, right)
    });
    match maximal.as_slice() {
        [selected] => {
            let selected = &candidates[*selected];
            ExtensionReferenceChoice::Selected(Box::new((
                selected.function.clone(),
                selected.plan.clone(),
                selected.type_arguments.clone(),
            )))
        }
        _ => ExtensionReferenceChoice::Ambiguous(
            maximal
                .into_iter()
                .map(|index| candidates[index].function.clone())
                .collect(),
        ),
    }
}

/// One specialized, applicable extension with its reference parameters, adaptation plan, and
/// inferred type arguments.
struct ApplicableExtension {
    function: FunctionInfo,
    parameters: Vec<Ty>,
    plan: Vec<AdaptedRefArgument>,
    type_arguments: Vec<Ty>,
}

fn applicable_extension(
    source: &dyn SymbolSource,
    mut function: FunctionInfo,
    receiver: Ty,
    unbound: bool,
    expected: Option<&'static FnSig>,
    rules: &impl ExtensionReferenceRules,
) -> Option<ApplicableExtension> {
    if !function.is_extension() {
        return None;
    }
    let context_count = function.context_count.min(function.semantic_params().len());
    let expected_values = match expected {
        Some(expected) => {
            let mut values = expected.params.to_vec();
            if unbound {
                values.get(context_count)?;
                values.remove(context_count);
            }
            Some(values)
        }
        None => None,
    };
    let type_arguments = specialize_candidate_with_type_arguments(
        source,
        &mut function,
        Some(receiver),
        expected_values.as_deref(),
        expected.map(|expected| expected.ret),
        |actual, bound| rules.bound_admits(actual, bound),
    )
    .ok()?;
    let declared_receiver = function.semantic_receiver()?;
    if !rules.assignable(receiver, declared_receiver) {
        crate::trace_compiler!(
            "resolve",
            "reject extension reference {}: receiver {receiver:?} is not assignable to {declared_receiver:?}",
            function.callable.name,
        );
        return None;
    }
    if !rules.accessible(&function) {
        return None;
    }
    let semantic_params = function.semantic_params();
    let value_params = &*semantic_params;
    let context_count = function.context_count.min(value_params.len());
    crate::trace_compiler!(
        "resolve",
        "extension reference candidate name={} unbound={unbound} receiver={receiver:?} declared_receiver={declared_receiver:?} params={value_params:?} required={} owner={} descriptor={}",
        function.callable.name,
        function.call_sig.required,
        function.callable.owner.render(),
        function.callable.descriptor,
    );
    let mut reference_params = value_params.to_vec();
    if unbound {
        reference_params.insert(context_count, receiver);
    }
    let plan = match expected {
        Some(expected) => {
            if function.callable.suspend && !expected.suspend {
                return None;
            }
            if unbound {
                let expected_receiver = *expected.params.get(context_count)?;
                if !super::callable_reference_selection::input_parameter_fits(
                    expected_receiver,
                    receiver,
                    |actual, target| rules.assignable(actual, target),
                ) {
                    return None;
                }
            }
            let mut plan = checked_parameter_plan(
                value_params,
                &function.call_sig,
                expected_values.as_deref()?,
                rules,
            )?;
            if unbound {
                shift_plan_values_from(&mut plan, context_count, 1);
                plan.insert(context_count, AdaptedRefArgument::Value(context_count));
            }
            // The plan has already mapped every input; only the result remains to fit.
            if !super::callable_reference_selection::result_is_compatible(
                function.callable.ret,
                expected,
                true,
                |actual, target| rules.assignable(actual, target),
            ) {
                return None;
            }
            plan
        }
        None => (0..reference_params.len())
            .map(AdaptedRefArgument::Value)
            .collect(),
    };
    Some(ApplicableExtension {
        function,
        parameters: reference_params,
        plan,
        type_arguments,
    })
}

/// Map a declaration's `params` onto an expected callable shape's inputs, then let the caller's
/// postponed-call evidence veto any input slot.
pub(super) fn checked_parameter_plan(
    params: &[Ty],
    call_sig: &CallSig,
    expected_params: &[Ty],
    rules: &impl ExtensionReferenceRules,
) -> Option<Vec<AdaptedRefArgument>> {
    let plan = parameter_plan(params, call_sig, expected_params, |actual, target| {
        rules.assignable(actual, target)
    })?;
    for (target, argument) in plan.iter().enumerate() {
        let declared = *params.get(target)?;
        match argument {
            AdaptedRefArgument::Value(value) => {
                if !rules.input_fits(*expected_params.get(*value)?, declared) {
                    return None;
                }
            }
            AdaptedRefArgument::Vararg {
                values,
                whole_array,
            } => {
                let target = if *whole_array {
                    declared
                } else {
                    declared.array_read_elem().unwrap_or(declared)
                };
                if values.iter().any(|value| {
                    expected_params
                        .get(*value)
                        .is_none_or(|expected| !rules.input_fits(*expected, target))
                }) {
                    return None;
                }
            }
            AdaptedRefArgument::Default => {}
        }
    }
    Some(plan)
}

impl ExtensionReferenceRules for super::Checker<'_> {
    fn bound_admits(&self, actual: Ty, bound: Ty) -> bool {
        self.generic_bound_admits(actual, bound)
    }

    fn assignable(&self, actual: Ty, target: Ty) -> bool {
        self.receiver_is_assignable(actual, target)
    }

    fn accessible(&self, function: &FunctionInfo) -> bool {
        crate::callable_access::source_callable_accessible(
            function.visibility,
            function.source_file,
            Some(self.file_index),
            || {
                self.libraries
                    .dependency_access_policy()
                    .internal_callable_accessible(
                        function.callable.external_identity,
                        function.callable.owner,
                    )
            },
        )
    }

    fn input_fits(&self, expected: Ty, target: Ty) -> bool {
        self.postponed_reference_input_fits(expected, target)
    }

    fn at_least_as_specific(&self, left: &[Ty], right: &[Ty]) -> bool {
        self.callable_ref_parameter_shape_at_least_as_specific(left, right)
    }
}
