//! Provider-neutral callable-reference specialization shared by body checking and compact
//! signature evaluation.

use crate::libraries::{CallSig, FunctionInfo};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdaptedRefArgument {
    Value(usize),
    /// A leading context parameter, supplied from the reference site rather than by the
    /// reference's own callers (`CallableReferencesToContextual`).
    Context,
    Default,
    Vararg {
        values: Vec<usize>,
        whole_array: bool,
        /// The single collected input is one kotlinc mapped as the whole array
        /// ([`CallSig::vararg_element_mapped_whole`]), so the reference reports no vararg
        /// conversion although it collects the input.
        mapped_whole: bool,
        /// The declaration's vararg parameter type after the reference's specialization, which
        /// a collected vararg is built as (`Array<out String>` for `vararg xs: E` with `E :=
        /// String`).
        array: Ty,
    },
}

/// Declaration-owned facts used to rank callable-reference adaptations after applicability.
/// `parameters` and `plan` use the same target-slot order; the plan identifies which declared
/// parameter types are actually exposed by the expected callable shape.
pub(super) struct AdaptedReferenceSpecificity<'a> {
    pub(super) parameters: &'a [Ty],
    pub(super) plan: &'a [AdaptedRefArgument],
    pub(super) is_vararg: bool,
}

/// The type each input mapped onto a vararg of type `declared` must supply: the whole array, or
/// one element read from it.
pub(super) fn vararg_input_type(declared: Ty, whole_array: bool) -> Ty {
    if whole_array {
        declared
    } else {
        declared.array_read_elem().unwrap_or(declared)
    }
}

/// Select the undominated adapted declarations, independently of where a provider found them.
/// Passed parameters decide ordinary specificity. Return compatibility belongs to applicability;
/// Kotlin's callable-reference conflict signature contains the adapted value-parameter types, not
/// the declaration's raw result. When all remaining passed shapes are mutually specific, Kotlin
/// prefers a non-vararg declaration and then the declaration with fewer slots (equivalently, fewer
/// omitted defaults for one expected shape).
pub(super) fn maximal_adapted_references(
    candidates: &[AdaptedReferenceSpecificity<'_>],
    mut shape_at_least_as_specific: impl FnMut(&[Ty], &[Ty]) -> bool,
) -> Vec<usize> {
    let shapes = candidates
        .iter()
        .map(|candidate| adaptation_specificity_shape(candidate.parameters, candidate.plan))
        .collect::<Vec<_>>();
    let mut maximal = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, _)| {
            let dominated = candidates.iter().enumerate().any(|(other_index, _)| {
                index != other_index
                    && shape_at_least_as_specific(&shapes[other_index], &shapes[index])
                    && !shape_at_least_as_specific(&shapes[index], &shapes[other_index])
            });
            (!dominated).then_some(index)
        })
        .collect::<Vec<_>>();
    if maximal.len() <= 1
        || !maximal.iter().all(|&left| {
            maximal
                .iter()
                .all(|&right| shape_at_least_as_specific(&shapes[left], &shapes[right]))
        })
    {
        return maximal;
    }
    let non_vararg = maximal
        .iter()
        .copied()
        .filter(|&index| !candidates[index].is_vararg)
        .collect::<Vec<_>>();
    if !non_vararg.is_empty() && non_vararg.len() < maximal.len() {
        maximal = non_vararg;
    }
    let Some(minimum) = maximal
        .iter()
        .map(|&index| candidates[index].parameters.len())
        .min()
    else {
        return maximal;
    };
    let shorter = maximal
        .iter()
        .copied()
        .filter(|&index| candidates[index].parameters.len() == minimum)
        .collect::<Vec<_>>();
    if !shorter.is_empty() && shorter.len() < maximal.len() {
        shorter
    } else {
        maximal
    }
}

/// A declaration's value parameters as overload specificity compares them: each of its own type
/// parameters erased to its upper bound.
pub(super) fn erased_declared_parameters(parameters: &[Ty]) -> Vec<Ty> {
    let unbound = crate::fx_hash::HashMap::<String, Ty>::default();
    parameters
        .iter()
        .map(|parameter| parameter.substitute_erased(&unbound))
        .collect()
}

fn adaptation_specificity_shape(params: &[Ty], plan: &[AdaptedRefArgument]) -> Vec<Ty> {
    let mut shape = Vec::new();
    for (index, argument) in plan.iter().enumerate() {
        let Some(&declared) = params.get(index) else {
            continue;
        };
        match argument {
            AdaptedRefArgument::Value(_) => shape.push(declared),
            AdaptedRefArgument::Context | AdaptedRefArgument::Default => {}
            AdaptedRefArgument::Vararg {
                values,
                whole_array: true,
                ..
            } => {
                if !values.is_empty() {
                    shape.push(declared);
                }
            }
            AdaptedRefArgument::Vararg { values, .. } => {
                let element = declared.array_read_elem().unwrap_or(declared);
                shape.extend(std::iter::repeat_n(element, values.len()));
            }
        }
    }
    shape
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
    /// [`CallSig::vararg_element_mapped_whole`] of the specialized declaration.
    pub(super) vararg_element_mapped_whole: bool,
    /// Final arguments in declaration-formal order. Selection owns inference; checked FIR merely
    /// publishes these bindings and lowering consumes them without reconstructing specialization
    /// from the adapted function shape.
    pub(super) type_arguments: Vec<Ty>,
}

fn alpha_equivalent_type(
    left: Ty,
    right: Ty,
    formals: &crate::fx_hash::HashMap<String, String>,
) -> bool {
    if left == right {
        return true;
    }
    match (left.kind(), right.kind()) {
        (TyKind::TyParam(left, _), TyKind::TyParam(right, _))
        | (TyKind::Var(left, _), TyKind::Var(right, _)) => formals
            .get(left.as_str())
            .is_some_and(|mapped| mapped == right),
        (TyKind::Nullable(left), TyKind::Nullable(right))
        | (TyKind::PlatformNullable(left), TyKind::PlatformNullable(right))
        | (TyKind::MutabilityFlexible(left), TyKind::MutabilityFlexible(right))
        | (TyKind::InProjection(left), TyKind::InProjection(right))
        | (TyKind::OutProjection(left), TyKind::OutProjection(right))
        | (TyKind::StarProjection(left), TyKind::StarProjection(right)) => {
            alpha_equivalent_type(left, right, formals)
        }
        (TyKind::Obj(left, left_arguments), TyKind::Obj(right, right_arguments)) => {
            left == right
                && left_arguments.len() == right_arguments.len()
                && left_arguments
                    .iter()
                    .zip(right_arguments)
                    .all(|(&left, &right)| alpha_equivalent_type(left, right, formals))
        }
        (TyKind::Fun(left), TyKind::Fun(right)) => {
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
) -> crate::fx_hash::HashSet<String> {
    let (
        TyKind::Obj(declared_owner, declared_arguments),
        TyKind::Obj(actual_owner, actual_arguments),
    ) = (
        declared_receiver.non_null().kind(),
        actual_receiver.non_null().kind(),
    )
    else {
        return crate::fx_hash::HashSet::default();
    };
    if declared_owner != actual_owner || declared_arguments.len() != actual_arguments.len() {
        return crate::fx_hash::HashSet::default();
    }
    let Some(classifier) = source.classifier(declared_owner) else {
        return crate::fx_hash::HashSet::default();
    };
    if classifier.type_params.len() != declared_arguments.len() {
        return crate::fx_hash::HashSet::default();
    }
    let mut alpha = crate::fx_hash::HashMap::default();
    for (&declared, classifier_formal) in declared_arguments.iter().zip(&classifier.type_params) {
        let declared = declared.projection_inner().unwrap_or(declared);
        if let TyKind::TyParam(method_formal, _) | TyKind::Var(method_formal, _) = declared.kind() {
            if generic.formals.contains(&method_formal) {
                alpha.insert(method_formal.to_string(), classifier_formal.clone());
            }
        }
    }
    let mut validated = crate::fx_hash::HashSet::default();
    for (index, (&declared, &actual)) in declared_arguments.iter().zip(actual_arguments).enumerate()
    {
        if actual.projection_inner().is_none() {
            continue;
        }
        let declared = declared.projection_inner().unwrap_or(declared);
        let (TyKind::TyParam(method_formal, _) | TyKind::Var(method_formal, _)) = declared.kind()
        else {
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

/// Whether an expected input can supply a whole vararg array: it must be the declared array's own
/// classifier (`Array<out E>` takes an `Array<…>`; `IntArray` takes an `IntArray`). Any other
/// input, including a primitive array against `Array<out E>`, is one collected vararg element.
fn same_array_classifier(actual: Ty, declared: Ty) -> bool {
    fn array_classifier(ty: Ty) -> Option<Ty> {
        match ty.non_null().kind() {
            TyKind::TyParam(_, bound) | TyKind::Var(_, bound) | TyKind::PlatformNullable(bound) => {
                array_classifier(bound)
            }
            TyKind::Obj(..) if ty.non_null().is_array() => Some(ty.non_null()),
            _ => None,
        }
    }
    match (array_classifier(actual), array_classifier(declared)) {
        (Some(actual), Some(declared)) => match (actual.kind(), declared.kind()) {
            (TyKind::Obj(actual, _), TyKind::Obj(declared, _)) => actual == declared,
            _ => false,
        },
        _ => false,
    }
}

/// What a reference is selected against: the function type's parameters and result, which exclude
/// the `bound_contexts` leading context parameters the reference site supplies.
#[derive(Clone, Copy)]
pub(super) struct ReferenceExpectation<'a> {
    pub(super) params: Option<&'a [Ty]>,
    pub(super) ret: Option<Ty>,
    pub(super) bound_contexts: usize,
    /// Which of `params` the enclosing call declares as one of its own type variables.
    pub(super) type_variable_inputs: TypeVariableInputs,
    /// The reproduced kotlinc profile of the compilation the reference is checked in.
    pub(super) profile: crate::kotlin_version::ReferenceProfile,
}

/// The symbols a reference is selected among and the reproduced kotlinc profile it is selected under.
#[derive(Clone, Copy)]
pub(super) struct ReferenceSource<'a> {
    pub(super) source: &'a dyn SymbolSource,
    pub(super) profile: crate::kotlin_version::ReferenceProfile,
}

impl<'a> ReferenceSource<'a> {
    pub(super) fn new(
        source: &'a dyn SymbolSource,
        reference: crate::kotlin_version::ReferenceProfile,
    ) -> Self {
        Self {
            source,
            profile: reference,
        }
    }
}

/// The inputs of an expected function type that the call passing the reference declares as one of
/// its own type variables (`f: (T) -> Int`), by position in `params`. A concrete input, one built
/// from a type variable (`(Array<T>) -> Int`), and an input a receiver already substituted
/// (`C<Array<String>>.run(f: (T) -> Int)`) are not type variables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TypeVariableInputs(u64);

impl TypeVariableInputs {
    pub(super) const NONE: Self = Self(0);

    /// The type-variable inputs of `declared`, the call's declared parameter type with only its
    /// receiver's type arguments substituted.
    pub(super) fn of_declared(declared: Ty) -> Self {
        let TyKind::Fun(function) = declared.non_null().kind() else {
            return Self::NONE;
        };
        let bits = function
            .params
            .iter()
            .take(u64::BITS as usize)
            .enumerate()
            .filter(|(_, input)| {
                matches!(
                    input.non_null().kind(),
                    TyKind::TyParam(..) | TyKind::Var(..)
                )
            })
            .fold(0, |bits, (index, _)| bits | 1 << index);
        Self(bits)
    }

    fn has(self, index: usize) -> bool {
        index < u64::BITS as usize && self.0 & 1 << index != 0
    }

    /// These inputs once the input at `index` is taken out of the expected parameters (an unbound
    /// extension reference's receiver).
    pub(super) fn without(self, index: usize) -> Self {
        if index >= u64::BITS as usize {
            return self;
        }
        let below = self.0 & ((1 << index) - 1);
        let above = (self.0 >> 1) & !((1 << index) - 1);
        Self(below | above)
    }
}

/// The function type an instance reference is selected against, with its type-variable inputs.
#[derive(Clone, Copy)]
pub(super) struct InstanceReferenceExpectation {
    pub(super) shape: &'static crate::types::FnSig,
    pub(super) type_variable_inputs: TypeVariableInputs,
}

impl From<&'static crate::types::FnSig> for InstanceReferenceExpectation {
    fn from(shape: &'static crate::types::FnSig) -> Self {
        Self {
            shape,
            type_variable_inputs: TypeVariableInputs::NONE,
        }
    }
}

/// Whether the expected input at `index` can supply a whole vararg array `declared`. kotlinc
/// reads an input its call declares as a type variable as one collected element on releases with
/// [`Quirk::VarargReferenceTakesTypeVariableInputAsElement`] (KT-90169), whatever array it
/// later binds to.
///
/// [`Quirk::VarargReferenceTakesTypeVariableInputAsElement`]: crate::kotlin_version::Quirk::VarargReferenceTakesTypeVariableInputAsElement
fn input_supplies_whole_vararg(
    profile: crate::kotlin_version::ReferenceProfile,
    inputs: TypeVariableInputs,
    index: usize,
    actual: Ty,
    declared: Ty,
) -> bool {
    !(inputs.has(index)
        && profile
            .has(crate::kotlin_version::Quirk::VarargReferenceTakesTypeVariableInputAsElement))
        && same_array_classifier(actual, declared)
}

/// Whether kotlinc maps the expected input at `index` onto a vararg `declared` as its whole array
/// before [`Quirk::VarargReferenceTakesTypeVariableInputAsElement`] (KT-90169) binds the vararg's
/// element type to that input. kotlinc's mapping takes any array, primitive arrays included, whole
/// when the vararg's element is the declaration's own type variable (`vararg xs: E` or `E?`); the
/// adapter still collects the input as one element, but reflection reports no vararg conversion.
/// An element that only mentions a type variable (`vararg xs: Array<E>`) or names none is mapped
/// element-wise, as on later releases.
///
/// [`Quirk::VarargReferenceTakesTypeVariableInputAsElement`]: crate::kotlin_version::Quirk::VarargReferenceTakesTypeVariableInputAsElement
fn element_input_mapped_whole(
    profile: crate::kotlin_version::ReferenceProfile,
    inputs: TypeVariableInputs,
    index: usize,
    actual: Ty,
    declared: Ty,
) -> bool {
    inputs.has(index)
        && profile.has(crate::kotlin_version::Quirk::VarargReferenceTakesTypeVariableInputAsElement)
        && actual.non_null().is_array()
        && declared
            .array_read_elem()
            .is_some_and(|element| matches!(element.non_null().kind(), TyKind::TyParam(..)))
}

pub(super) fn specialize_generic_signature(
    source: &dyn SymbolSource,
    generic: &crate::libraries::GenericSig,
    call_sig: &CallSig,
    extension_receiver: Option<Ty>,
    expectation: ReferenceExpectation<'_>,
    mut is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> Result<SpecializedGenericCallableReference, CallableRefSpecialization> {
    let ReferenceExpectation {
        params: expected_params,
        ret: expected_ret,
        bound_contexts,
        type_variable_inputs,
        profile,
    } = expectation;
    let mut bindings = crate::symbol_resolver::GSigBinds::default();
    let mut element_mapped_whole = false;
    if let (Some(declared), Some(actual)) = (generic.receiver, extension_receiver) {
        let mut inferred = crate::symbol_resolver::GSigBinds::default();
        crate::symbol_resolver::unify_ty_from_symbols(source, declared, actual, &mut inferred);
        crate::symbol_resolver::merge_generic_bindings(generic, 0, &mut bindings, inferred);
    }
    if let Some(expected) = expected_params {
        let vararg = call_sig.vararg_index;
        // A whole array is the vararg's single input. When it is the last expected input, any
        // parameters after the vararg take their defaults (`vararg xs: E, k: Int = 0`).
        let last_vararg_input = vararg.and_then(|index| {
            let expected_index = index.checked_sub(bound_contexts)?;
            if expected.len() != expected_index + 1 {
                return None;
            }
            Some((
                expected_index,
                *expected.get(expected_index)?,
                *generic.params.get(index)?,
            ))
        });
        let whole_vararg = last_vararg_input.is_some_and(|(index, actual, declared)| {
            input_supplies_whole_vararg(profile, type_variable_inputs, index, actual, declared)
        });
        element_mapped_whole = last_vararg_input.is_some_and(|(index, actual, declared)| {
            element_input_mapped_whole(profile, type_variable_inputs, index, actual, declared)
        });
        let actuals = expected
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, actual)| {
                let index = index + bound_contexts;
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
            &mut is_assignable,
        ) {
            crate::symbol_resolver::merge_generic_upper_bindings(
                generic,
                0,
                &mut bindings,
                inferred,
                &mut is_assignable,
            );
        }
    }
    let receiver_validated = match (generic.receiver, extension_receiver) {
        (Some(declared), Some(actual)) => {
            receiver_validated_projected_formals(source, generic, declared, actual)
        }
        _ => crate::fx_hash::HashSet::default(),
    };
    let mut validation_bindings = bindings.clone();
    for formal in receiver_validated {
        validation_bindings.remove(formal.as_str());
    }
    if !crate::symbol_resolver::generic_bindings_satisfy_bounds(
        generic,
        &validation_bindings,
        is_assignable,
    ) {
        return Err(CallableRefSpecialization::Inapplicable);
    }
    if generic
        .formals
        .iter()
        .any(|formal| !bindings.contains_key(formal.as_str()))
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
        vararg_element_mapped_whole: element_mapped_whole,
        ret: crate::symbol_resolver::specialize_signature_output_type(
            source,
            generic.ret,
            &bindings,
        ),
        type_arguments: generic
            .formals
            .iter()
            .map(|formal| bindings[formal.as_str()])
            .collect(),
    })
}

pub(super) fn specialize_candidate_with_type_arguments(
    source: &dyn SymbolSource,
    function: &mut FunctionInfo,
    extension_receiver: Option<Ty>,
    expectation: ReferenceExpectation<'_>,
    is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> Result<Vec<Ty>, CallableRefSpecialization> {
    let Some(generic) = function.generic_sig.clone() else {
        return Ok(Vec::new());
    };
    let specialized = specialize_generic_signature(
        source,
        &generic,
        &function.call_sig,
        extension_receiver,
        expectation,
        is_assignable,
    )?;
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
    function.call_sig.vararg_element_mapped_whole = specialized.vararg_element_mapped_whole;
    function.generic_sig = None;
    Ok(type_arguments)
}

pub(super) fn specialize_candidate(
    reference: ReferenceSource<'_>,
    function: &mut FunctionInfo,
    bound_contexts: usize,
    extension_receiver: Option<Ty>,
    expected_params: Option<&[Ty]>,
    expected_ret: Option<Ty>,
    is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> CallableRefSpecialization {
    specialize_candidate_with_type_arguments(
        reference.source,
        function,
        extension_receiver,
        ReferenceExpectation {
            params: expected_params,
            ret: expected_ret,
            bound_contexts,
            type_variable_inputs: TypeVariableInputs::NONE,
            profile: reference.profile,
        },
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
    let mut result_bindings = crate::symbol_resolver::GSigBinds::default();
    crate::symbol_resolver::unify_inferred_ty(expected.ret, ret, &mut result_bindings);
    expected
        .params
        .iter()
        .zip(params)
        .all(|(expected, actual)| {
            let expected =
                crate::symbol_resolver::ty_subst_keep_unbound(*expected, &result_bindings);
            match expected.non_null().kind() {
                TyKind::TyParam(_, bound) | TyKind::Var(_, bound) => is_assignable(*actual, bound),
                _ => is_assignable(expected, *actual),
            }
        })
        && result_is_compatible(ret, expected, allow_unit_coercion, is_assignable)
}

/// Whether a declaration's `ret` fits an expected callable shape's result. An open result variable
/// is bound by that same result first, so only its other occurrences can contradict it.
pub(super) fn result_is_compatible(
    ret: Ty,
    expected: &'static crate::types::FnSig,
    allow_unit_coercion: bool,
    mut is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> bool {
    let mut result_bindings = crate::symbol_resolver::GSigBinds::default();
    crate::symbol_resolver::unify_inferred_ty(expected.ret, ret, &mut result_bindings);
    let expected_ret =
        crate::symbol_resolver::ty_subst_keep_unbound(expected.ret, &result_bindings);
    (allow_unit_coercion && expected.ret == Ty::Unit) || is_assignable(ret, expected_ret)
}

/// Whether each input occurrence of an outer variable that the result binds agrees with that
/// binding. One candidate is one constraint system: `Int -> String` cannot satisfy `(T) -> T`, since
/// the result requires `String <: T` and the input `T <: Int`. Inputs whose variables the result
/// leaves open were already admitted against their bounds.
pub(super) fn inputs_agree_with_result(
    ret: Ty,
    expected: &'static crate::types::FnSig,
    target_params: &[Ty],
    plan: &[AdaptedRefArgument],
    mut is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> bool {
    let mut result_bindings = crate::symbol_resolver::GSigBinds::default();
    crate::symbol_resolver::unify_inferred_ty(expected.ret, ret, &mut result_bindings);
    if result_bindings.is_empty() {
        return true;
    }
    let mut fits = |input: Ty, declared: Ty| {
        let bound = crate::symbol_resolver::ty_subst_keep_unbound(input, &result_bindings);
        bound == input
            || matches!(bound.non_null().kind(), TyKind::TyParam(..))
            || is_assignable(bound, declared)
    };
    plan.iter().enumerate().all(|(target, argument)| {
        let Some(&declared) = target_params.get(target) else {
            return true;
        };
        match argument {
            AdaptedRefArgument::Value(value) => expected
                .params
                .get(*value)
                .is_none_or(|&input| fits(input, declared)),
            AdaptedRefArgument::Vararg {
                values,
                whole_array,
                ..
            } => {
                let declared = vararg_input_type(declared, *whole_array);
                values.iter().all(|value| {
                    expected
                        .params
                        .get(*value)
                        .is_none_or(|&input| fits(input, declared))
                })
            }
            AdaptedRefArgument::Context | AdaptedRefArgument::Default => true,
        }
    })
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
    // Each occurrence of an outer inference variable is a separate constraint. Sharing one eager
    // substitution map across input and result positions turns a contradictory target such as
    // `String -> Int` under `(T) -> T` into one widened placeholder and hides the contradiction
    // from the enclosing call solver. Materialize every occurrence independently; the ordinary
    // variance-aware call constraints then combine the resulting input uppers and result lowers.
    let specialize_occurrence = |shape: Ty, actual: Ty| {
        let mut bindings = crate::symbol_resolver::GSigBinds::default();
        crate::symbol_resolver::unify_inferred_ty(shape, actual, &mut bindings);
        crate::symbol_resolver::ty_subst_keep_unbound(shape, &bindings)
    };
    let mut parameters = expected.params.clone();
    for (target, argument) in plan.iter().enumerate() {
        let Some(target_parameter) = target_params.get(target).copied() else {
            continue;
        };
        match argument {
            AdaptedRefArgument::Value(value) => {
                if let Some(parameter) = parameters.get_mut(*value) {
                    *parameter = specialize_occurrence(*parameter, target_parameter);
                }
            }
            AdaptedRefArgument::Vararg {
                values,
                whole_array,
                ..
            } => {
                let parameter = vararg_input_type(target_parameter, *whole_array);
                for value in values {
                    if let Some(expected_parameter) = parameters.get_mut(*value) {
                        *expected_parameter = specialize_occurrence(*expected_parameter, parameter);
                    }
                }
            }
            AdaptedRefArgument::Context | AdaptedRefArgument::Default => {}
        }
    }
    let result = specialize_occurrence(expected.ret, target_ret);
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
    match (expected.non_null().kind(), target.non_null().kind()) {
        (TyKind::TyParam(_, bound) | TyKind::Var(_, bound), _) => is_assignable(target, bound),
        (_, TyKind::TyParam(_, bound) | TyKind::Var(_, bound)) => is_assignable(expected, bound),
        _ => is_assignable(expected, target),
    }
}

/// [`parameter_plan`] for a reference that binds no context arguments.
pub(super) fn plain_parameter_plan(
    params: &[Ty],
    call_sig: &CallSig,
    expected_params: &[Ty],
    is_assignable: impl FnMut(Ty, Ty) -> bool,
) -> Option<Vec<AdaptedRefArgument>> {
    parameter_plan(params, call_sig, 0, expected_params, is_assignable)
}

pub(super) fn parameter_plan(
    params: &[Ty],
    call_sig: &CallSig,
    context_count: usize,
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
                    mapped_whole: false,
                    array,
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
                    mapped_whole: call_sig.vararg_element_mapped_whole
                        && count == 1
                        && value + 1 == expected_params.len(),
                    array,
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
                    mapped_whole: false,
                    array,
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

    // Leading context parameters are filled from the reference site; the expected shape maps onto
    // the remaining declaration inputs only.
    let context_count = context_count.min(params.len());
    let mut plan = Vec::with_capacity(params.len());
    plan.extend(std::iter::repeat_n(
        AdaptedRefArgument::Context,
        context_count,
    ));
    map_from(
        params,
        call_sig,
        expected_params,
        context_count,
        0,
        &mut plan,
        &mut is_assignable,
    )
    .then_some(plan)
}

/// Select an expected-shape instance reference from ordinary member candidates.
///
/// Body checking and compact-signature evaluation must make the same adaptation, generic
/// specialization, receiver-rank, duplicate-fact, and specificity decision. Keeping that
/// decision here also lets signature evaluation try members before it demands any extension
/// candidate whose own inferred signature may currently be under computation.
fn select_adapted_bound_instance_candidate(
    source: &dyn SymbolSource,
    profile: crate::kotlin_version::ReferenceProfile,
    overloads: &[FunctionInfo],
    bind_contexts: bool,
    expected: InstanceReferenceExpectation,
    mut is_assignable: impl FnMut(Ty, Ty) -> bool,
    shape_at_least_as_specific: impl FnMut(&[Ty], &[Ty]) -> bool,
) -> Option<(FunctionInfo, Vec<AdaptedRefArgument>, Vec<Ty>)> {
    let InstanceReferenceExpectation {
        shape: expected,
        type_variable_inputs,
    } = expected;
    let mut candidates = overloads
        .iter()
        .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
        .cloned()
        .filter_map(|mut candidate| {
            if candidate.callable.suspend && !expected.suspend {
                return None;
            }
            let bound_contexts = if bind_contexts {
                candidate.context_count
            } else {
                0
            };
            let declared = candidate.semantic_params().into_owned();
            let type_arguments = specialize_candidate_with_type_arguments(
                source,
                &mut candidate,
                None,
                ReferenceExpectation {
                    params: Some(&expected.params),
                    ret: Some(expected.ret),
                    bound_contexts,
                    type_variable_inputs,
                    profile,
                },
                &mut is_assignable,
            )
            .ok()?;
            let plan = parameter_plan(
                &candidate.semantic_params(),
                &candidate.call_sig,
                if bind_contexts {
                    candidate.context_count
                } else {
                    0
                },
                &expected.params,
                &mut is_assignable,
            )?;
            if !is_compatible(
                &expected.params,
                candidate.callable.ret,
                candidate.callable.suspend,
                expected,
                true,
                &mut is_assignable,
            ) {
                return None;
            }
            Some((candidate, plan, type_arguments, declared))
        })
        .collect::<Vec<_>>();
    let nearest = candidates
        .iter()
        .map(|candidate| candidate.0.receiver_rank)
        .min()?;
    candidates.retain(|candidate| candidate.0.receiver_rank == nearest);
    // Providers may expose one inherited declaration through multiple hierarchy paths. Those are
    // duplicate facts; distinct declared signatures remain overload candidates, even when this
    // expected shape instantiates them alike (`pick(x: T)` and `pick(x: Holder<T>)`).
    let mut unique = Vec::<(FunctionInfo, Vec<AdaptedRefArgument>, Vec<Ty>, Vec<Ty>)>::new();
    for candidate in candidates {
        if unique.iter().any(|existing| {
            existing.3 == candidate.3
                && existing.0.callable.ret == candidate.0.callable.ret
                && existing.0.callable.suspend == candidate.0.callable.suspend
        }) {
            continue;
        }
        unique.push(candidate);
    }
    let declared_shapes = unique
        .iter()
        .map(|candidate| erased_declared_parameters(&candidate.3))
        .collect::<Vec<_>>();
    let specificity = unique
        .iter()
        .zip(&declared_shapes)
        .map(
            |((candidate, plan, ..), parameters)| AdaptedReferenceSpecificity {
                parameters,
                plan,
                is_vararg: candidate.call_sig.vararg_index.is_some(),
            },
        )
        .collect::<Vec<_>>();
    let maximal = maximal_adapted_references(&specificity, shape_at_least_as_specific);
    let [selected] = maximal.as_slice() else {
        return None;
    };
    let (selected, plan, type_arguments, _) = unique.swap_remove(*selected);
    Some((selected, plan, type_arguments))
}

/// Select an adapted instance reference. `unbound_receiver` is present for
/// `Classifier::member`: its leading contextual input supplies the dispatch receiver, while the
/// returned argument plan owns only declaration value parameters. Bound and unbound callers share
/// this entry point so they cannot drift into different specialization or overload-selection rules.
pub(super) fn select_adapted_instance_candidate(
    reference: ReferenceSource<'_>,
    overloads: &[FunctionInfo],
    bind_contexts: bool,
    unbound_receiver: Option<Ty>,
    expected: impl Into<InstanceReferenceExpectation>,
    mut is_assignable: impl FnMut(Ty, Ty) -> bool,
    shape_at_least_as_specific: impl FnMut(&[Ty], &[Ty]) -> bool,
) -> Option<(FunctionInfo, Vec<AdaptedRefArgument>, Vec<Ty>)> {
    let InstanceReferenceExpectation {
        shape: expected,
        type_variable_inputs,
    } = expected.into();
    let type_variable_inputs = match unbound_receiver {
        Some(_) => type_variable_inputs.without(0),
        None => type_variable_inputs,
    };
    let method = match unbound_receiver {
        Some(receiver) => {
            let (&expected_receiver, expected_values) = expected.params.split_first()?;
            if !input_parameter_fits(expected_receiver, receiver, &mut is_assignable) {
                return None;
            }
            let TyKind::Fun(method) = Ty::fun_with_shape(
                expected_values.to_vec(),
                expected.ret,
                expected.context_count,
                false,
                expected.suspend,
            )
            .kind() else {
                unreachable!("unbound member expectation is always a function")
            };
            method
        }
        None => expected,
    };
    let (selected, mut plan, type_arguments) = select_adapted_bound_instance_candidate(
        reference.source,
        reference.profile,
        overloads,
        bind_contexts,
        InstanceReferenceExpectation {
            shape: method,
            type_variable_inputs,
        },
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

pub(super) fn plan_is_identity_from(plan: &[AdaptedRefArgument], value_offset: usize) -> bool {
    plan.iter().enumerate().all(|(target, argument)| {
        matches!(argument, AdaptedRefArgument::Value(value) if *value == target + value_offset)
    })
}

pub(super) fn shift_plan_values(plan: &mut [AdaptedRefArgument], offset: usize) {
    shift_plan_values_from(plan, 0, offset);
}

/// Remove a synthetic enclosing receiver from an extension-style reference plan. Constructor
/// references carry that receiver separately; their value indices exclude its input slot.
pub(super) fn constructor_plan_without_receiver(
    mut plan: Vec<AdaptedRefArgument>,
    receiver: usize,
) -> Option<Vec<AdaptedRefArgument>> {
    if plan.get(receiver) != Some(&AdaptedRefArgument::Value(receiver)) {
        return None;
    }
    plan.remove(receiver);
    let without_receiver = |value: &mut usize| {
        if *value == receiver {
            return None;
        }
        if *value > receiver {
            *value -= 1;
        }
        Some(())
    };
    for argument in &mut plan {
        match argument {
            AdaptedRefArgument::Value(value) => without_receiver(value)?,
            AdaptedRefArgument::Vararg { values, .. } => {
                for value in values {
                    without_receiver(value)?;
                }
            }
            AdaptedRefArgument::Context | AdaptedRefArgument::Default => {}
        }
    }
    Some(plan)
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
            AdaptedRefArgument::Value(_)
            | AdaptedRefArgument::Context
            | AdaptedRefArgument::Default => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin_version::{Quirk, ReferenceProfile};

    fn formal(name: &str) -> Ty {
        Ty::ty_param(name, Ty::nullable(Ty::obj("kotlin/Any")))
    }

    #[test]
    fn a_bare_or_nullable_formal_input_is_a_type_variable_input() {
        let t = formal("call:T");
        let declared = Ty::fun(vec![t, Ty::nullable(t), Ty::array(t), Ty::String], Ty::Int);
        let inputs = TypeVariableInputs::of_declared(declared);
        assert_eq!(
            (0..5).map(|index| inputs.has(index)).collect::<Vec<_>>(),
            [true, true, false, false, false]
        );
        assert_eq!(
            TypeVariableInputs::of_declared(Ty::nullable(declared)),
            inputs
        );
        assert_eq!(TypeVariableInputs::of_declared(t), TypeVariableInputs::NONE);
    }

    #[test]
    fn removing_an_input_shifts_the_later_ones() {
        let inputs = TypeVariableInputs(0b1011);
        assert_eq!(inputs.without(0), TypeVariableInputs(0b101));
        assert_eq!(inputs.without(1), TypeVariableInputs(0b101));
        assert_eq!(inputs.without(2), TypeVariableInputs(0b111));
        assert_eq!(inputs.without(64), inputs);
    }

    #[test]
    fn a_type_variable_input_takes_the_whole_vararg_only_without_the_quirk() {
        let quirk = Quirk::VarargReferenceTakesTypeVariableInputAsElement;
        let with = ReferenceProfile::default().with_quirk(quirk, true);
        let without = with.with_quirk(quirk, false);
        let actual = Ty::array(Ty::String);
        let declared = Ty::array(Ty::out_projection(formal("ref:E")));
        let from_variable = TypeVariableInputs(1);
        assert!(!input_supplies_whole_vararg(
            with,
            from_variable,
            0,
            actual,
            declared
        ));
        assert!(input_supplies_whole_vararg(
            without,
            from_variable,
            0,
            actual,
            declared
        ));
        // A concrete input takes the whole array either way.
        for profile in [with, without] {
            assert!(input_supplies_whole_vararg(
                profile,
                TypeVariableInputs::NONE,
                0,
                actual,
                declared
            ));
        }
    }

    #[test]
    fn an_array_type_variable_input_maps_whole_onto_a_type_variable_element_under_the_quirk() {
        let quirk = Quirk::VarargReferenceTakesTypeVariableInputAsElement;
        let with = ReferenceProfile::default().with_quirk(quirk, true);
        let without = with.with_quirk(quirk, false);
        let element = formal("ref:E");
        let vararg_of = |element| Ty::array(Ty::out_projection(element));
        let from_variable = TypeVariableInputs(1);
        let mapped = |profile, inputs, actual, declared| {
            element_input_mapped_whole(profile, inputs, 0, actual, declared)
        };
        let strings = Ty::array(Ty::String);
        let ints = Ty::obj("kotlin/IntArray");
        // Any array, primitive arrays and a nullable input included, against `E` or `E?`.
        for actual in [strings, ints, Ty::nullable(strings)] {
            assert!(mapped(with, from_variable, actual, vararg_of(element)));
            assert!(mapped(
                with,
                from_variable,
                actual,
                vararg_of(Ty::nullable(element))
            ));
        }
        // Not without the quirk, for a concrete input, a non-array input, or an element that only
        // mentions the type variable or names none.
        assert!(!mapped(without, from_variable, strings, vararg_of(element)));
        assert!(!mapped(
            with,
            TypeVariableInputs::NONE,
            strings,
            vararg_of(element)
        ));
        assert!(!mapped(with, from_variable, Ty::String, vararg_of(element)));
        assert!(!mapped(
            with,
            from_variable,
            strings,
            vararg_of(Ty::array(element))
        ));
        assert!(!mapped(with, from_variable, strings, vararg_of(strings)));
    }

    #[test]
    fn repeated_outer_formal_preserves_distinct_reference_constraints() {
        let formal = Ty::ty_param("outer:T", Ty::nullable(Ty::obj("kotlin/Any")));
        let TyKind::Fun(expected) = Ty::fun(vec![formal], formal).kind() else {
            unreachable!()
        };

        let realized = realize_expected_shape(
            expected,
            &[Ty::String],
            Ty::Int,
            &[AdaptedRefArgument::Value(0)],
        );

        assert_eq!(realized, Ty::fun(vec![Ty::String], Ty::Int));
    }
}
