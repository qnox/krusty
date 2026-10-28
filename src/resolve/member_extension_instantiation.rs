//! Instantiating one member-extension declaration against a call: binding its method formals from
//! the extension receiver, explicit type arguments and argument types, then scoring applicability.
//! Candidate collection and selection over the instantiated shapes live in
//! `member_extension_selection`.

use super::*;

/// The caller-owned views a member-extension selection consults: how implicit context arguments
/// are found, and how source arguments are inspected and scored. Signature solving has no
/// expressions, so it supplies probes that never dereference argument syntax.
#[derive(Clone, Copy)]
pub(crate) struct MemberExtensionProbes<'p> {
    pub(super) explicit_context_arguments: bool,
    /// Variables owned by an active postponed-call frame. A nested candidate may expose them in an
    /// applied parameter but must not consume their constraints before the owning frame sees them.
    pub(super) deferred_type_variables: &'p [String],
    pub(super) select_context_arguments: &'p ContextArgumentSelector<'p>,
    pub(super) is_spread_arg: &'p dyn Fn(usize) -> bool,
    pub(super) unit_coerced_lambda: &'p dyn Fn(usize, Ty, Ty) -> Option<Ty>,
    pub(super) score_candidate: &'p CandidateScorer<'p>,
}

/// Scores one candidate's parameters against the call's argument slots; `None` is inapplicable.
pub(super) type CandidateScorer<'s> =
    dyn Fn(&[Ty], &CallSig, ArgSlots<'_>) -> Option<CallCandidateScore> + 's;

/// Instantiate one member-extension shape against a call: bind method formals from the receiver and
/// the argument types, then score applicability. Argument SYNTAX is reached only through the
/// caller-supplied probes, so a caller with no expressions (signature solving in Pass 1, where the
/// only member-extension reads are zero-argument) passes probes that are never consulted.
pub(super) fn instantiate_member_extension_with(
    source: &dyn SymbolSource,
    probes: MemberExtensionProbes<'_>,
    result_constraint: CallResultConstraint,
    inference_bindings: Option<&crate::symbol_resolver::GSigBinds>,
    shape: &MemberExtensionFunctionShape,
    call: MemberExtensionCall<'_>,
) -> Option<InstantiatedMemberExtension> {
    let MemberExtensionProbes {
        explicit_context_arguments,
        deferred_type_variables,
        select_context_arguments,
        is_spread_arg,
        unit_coerced_lambda,
        score_candidate,
    } = probes;
    let MemberExtensionCall {
        extension_receiver,
        name,
        args,
        partial_arg_tys,
        arg_names,
        explicit_type_args,
        trailing_lambda,
    } = call;
    let function = &shape.function;
    let generic = function.signature.generic_sig.as_ref();
    let method_type_params = generic
        .map(|signature| signature.formals.iter().enumerate().collect::<Vec<_>>())
        .unwrap_or_default();
    if !explicit_type_args.is_empty() && explicit_type_args.len() != method_type_params.len() {
        return None;
    }
    let mut full_call_sig = function.signature.call_sig();
    full_call_sig.reified_type_parameter_ordinals =
        function.reified_type_parameter_ordinals.clone();
    let mut bindings = shape.class_bindings.clone();
    let mut lower_inputs = HashMap::default();
    for (_, parameter) in &method_type_params {
        bindings.remove(*parameter);
    }
    if let Some(inference_bindings) = inference_bindings {
        for (_, parameter) in &method_type_params {
            if let Some(actual) = inference_bindings.get(*parameter) {
                bindings.insert((*parameter).clone(), *actual);
            }
        }
    }
    for ((_, parameter), argument) in method_type_params.iter().zip(explicit_type_args) {
        bindings.insert((*parameter).clone(), *argument);
    }
    // Both receiver and parameter inference consume the provider-normalized semantic signature.
    // The same `Ty::TyParam` unifier therefore handles source syntax and decoded metadata; neither
    // path needs to retain or reconstruct a provider-specific spelling. A receiver whose classifier
    // differs from the declared one constrains the formals through its applied supertype, as an
    // ordinary extension receiver does (`List<String>` binds `T` of `Iterable<T>`).
    let declared_receiver = generic
        .and_then(|signature| signature.receiver)
        .unwrap_or(function.receiver_ty);
    intersection_inference::record_direct_lower_input(
        generic,
        declared_receiver,
        extension_receiver,
        &mut lower_inputs,
    );
    crate::symbol_resolver::unify_ty_from_symbols(
        source,
        declared_receiver,
        extension_receiver,
        &mut bindings,
    );
    let declared_params = generic.map_or(function.signature.params.as_slice(), |signature| {
        signature.params.as_slice()
    });
    let initial_params = declared_params
        .iter()
        .map(|parameter| crate::symbol_resolver::ty_subst(*parameter, &bindings))
        .collect::<Vec<_>>();
    let ContextualCallShape {
        params: _,
        call_sig,
        parameter_indices,
        context_sources,
        context_actual_types,
    } = contextual_call_shape_with(
        explicit_context_arguments,
        select_context_arguments,
        &initial_params,
        &full_call_sig,
        function.signature.context_count,
        arg_names,
    )
    .ok()?;
    // Context values are ordinary type-inference evidence. Their source is selected by the same
    // scope query as every contextual call, while their types constrain method formals here.
    for (parameter, actual) in context_actual_types.iter().enumerate() {
        let Some(actual) = actual else { continue };
        let declared = declared_params[parameter];
        intersection_inference::record_direct_lower_input(
            generic,
            declared,
            *actual,
            &mut lower_inputs,
        );
        unify_member_extension_slot(declared, *actual, deferred_type_variables, &mut bindings);
    }
    // Use the common index mapper over the SOURCE-VISIBLE shape. Implicit context parameters are
    // absent; explicitly named ones are present at the end and map back through
    // `parameter_indices` to their declaration slots.
    let argument_parameters = call_argument_parameter_indices(
        args.len(),
        parameter_indices.len(),
        arg_names,
        trailing_lambda,
        &call_sig,
    )?
    .into_iter()
    .enumerate()
    .map(|(source, parameter)| (parameter, source))
    .collect::<Vec<_>>();
    for &(visible_parameter, source) in &argument_parameters {
        let Some(Some(actual)) = partial_arg_tys.get(source) else {
            continue;
        };
        let parameter = *parameter_indices.get(visible_parameter)?;
        let Some(&declared) = declared_params.get(parameter) else {
            continue;
        };
        let vararg = full_call_sig.vararg_index == Some(parameter);
        // Generic signatures describe a vararg slot as its ARRAY (`Array<T>`), while call-site
        // inference is over its element. A spread contributes the source array's element and an
        // ordinary argument contributes its own type. Normalizing both sides to the element keeps
        // `render(*intArray)` and `render(1)` on one inference path and works for non-final varargs.
        let declared = if vararg {
            declared.array_read_elem().unwrap_or(declared)
        } else {
            declared
        };
        let actual = if vararg && is_spread_arg(source) {
            actual.array_read_elem().unwrap_or(*actual)
        } else {
            *actual
        };
        intersection_inference::record_direct_lower_input(
            generic,
            declared,
            actual,
            &mut lower_inputs,
        );
        unify_member_extension_slot(declared, actual, deferred_type_variables, &mut bindings);
    }
    if let Some(expected) = result_constraint.expected() {
        let declared_ret = generic.map_or(function.signature.ret, |signature| signature.ret);
        let declared_ret = result_constraint.declaration_result(declared_ret);
        unify_member_extension_slot(
            declared_ret,
            expected,
            deferred_type_variables,
            &mut bindings,
        );
    }
    // A formal observable only through another formal's bound (`T` of `E : EntityID<T>` on a
    // `Column<out E?>` receiver) is solved from that formal's applied bound, as for any other
    // generic call, before it can fall back to its own bound.
    if let Some(generic) = generic {
        crate::symbol_resolver::resolve_bound_violating_bindings(
            Some(source),
            generic,
            &mut bindings,
            explicit_type_args,
        );
    }
    // The call's evidence so far, before any unsolved formal stands for its bound: a lambda
    // argument is shaped from THIS view, so a formal only a lambda can solve stays open for the
    // call's postponed-lambda analysis instead of being fixed to its bound ahead of that lambda.
    let open_formals = method_type_params
        .iter()
        .filter(|(_, formal)| !bindings.contains_key(*formal))
        .map(|(_, formal)| (*formal).clone())
        .collect::<Vec<_>>();
    let visible_params_under = |bindings: &crate::symbol_resolver::GSigBinds| {
        let mut logical_params = declared_params
            .iter()
            .map(|parameter| apply_inference_bindings(*parameter, bindings))
            .collect::<Vec<_>>();
        // The selected-call handoff stores a vararg's logical ELEMENT because lowering packs
        // source expressions into the physical array. Candidate scoring, however, consumes the
        // declared ARRAY shape so it can distinguish an element from a spread array. Convert
        // exactly once at this semantic boundary regardless of whether the provider supplied
        // source syntax or metadata.
        if let Some(index) = full_call_sig.vararg_index {
            let parameter = logical_params.get_mut(index)?;
            *parameter = parameter.array_read_elem().unwrap_or(*parameter);
        }
        let visible_params = parameter_indices
            .iter()
            .map(|&parameter| logical_params[parameter])
            .collect::<Vec<_>>();
        Some((logical_params, visible_params))
    };
    let (_, open_visible_params) = visible_params_under(&bindings)?;
    for (index, parameter) in method_type_params {
        let fallback = generic
            .and_then(|signature| signature.formal_bounds.get(index))
            .and_then(|bounds| bounds.first())
            .copied()
            .map(|bound| crate::symbol_resolver::unsolved_formal_bound(bound, parameter, &bindings))
            .unwrap_or_else(|| Ty::obj("kotlin/Any"));
        bindings.entry(parameter.clone()).or_insert(fallback);
    }
    let (logical_params, visible_params) = visible_params_under(&bindings)?;
    let mut applicability_params = visible_params.clone();
    if let Some(index) = call_sig.vararg_index {
        let parameter = applicability_params.get_mut(index)?;
        *parameter = Ty::array(*parameter);
    }
    let mut applicability_arg_tys = partial_arg_tys.to_vec();
    for &(parameter, source) in &argument_parameters {
        let (Some(&expected), Some(Some(actual))) = (
            visible_params.get(parameter),
            applicability_arg_tys.get(source).copied(),
        ) else {
            continue;
        };
        if let Some(coerced) = unit_coerced_lambda(source, expected, actual) {
            applicability_arg_tys[source] = Some(coerced);
        }
    }
    let mut member = crate::libraries::LibraryMember::new(
        name.to_string(),
        applicability_params,
        function.signature.ret,
        String::new(),
    );
    member.call_sig = call_sig.clone();
    let score = score_candidate(
        &member.params,
        &member.call_sig,
        ArgSlots {
            args,
            partial_arg_tys: &applicability_arg_tys,
            arg_names,
            trailing_lambda,
        },
    )?
    .rank;
    let ret = apply_inference_bindings(
        generic.map_or(function.signature.ret, |signature| signature.ret),
        &bindings,
    );
    // Ordinary extension and member calls publish these solutions on the call. A member
    // extension publishes them too, so an inlined `typeOf<T>()` receives the call-site argument.
    let type_arguments = generic
        .map(|signature| {
            signature
                .formals
                .iter()
                .map(|formal| {
                    member_extension_selection::solved_member_extension_type_argument(
                        &bindings, formal,
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(InstantiatedMemberExtension {
        extension_receiver: apply_inference_bindings(declared_receiver, &bindings),
        logical_params,
        visible_params,
        open_visible_params,
        open_formals,
        context_sources,
        ret,
        call_sig,
        physical_vararg_index: full_call_sig.vararg_index,
        score,
        argument_parameters,
        type_arguments,
        bindings,
        lower_inputs,
        parameter_indices,
    })
}

pub(super) struct InstantiatedMemberExtension {
    pub(super) extension_receiver: Ty,
    /// Full semantic parameter list: leading contexts followed by value parameters.
    pub(super) logical_params: Vec<Ty>,
    pub(super) visible_params: Vec<Ty>,
    /// The source-visible parameters under the call's evidence alone: a method formal no
    /// receiver, explicit type argument, typed argument, or expected result has solved is still
    /// itself here, where `visible_params` has its bound standing in for it.
    pub(super) open_visible_params: Vec<Ty>,
    /// The method formals that evidence leaves open in `open_visible_params`.
    pub(super) open_formals: Vec<String>,
    pub(super) context_sources: Vec<Option<ResolvedContextArgument>>,
    pub(super) ret: Ty,
    pub(super) call_sig: crate::libraries::CallSig,
    pub(super) physical_vararg_index: Option<usize>,
    pub(super) score: (usize, std::cmp::Reverse<usize>, bool),
    pub(super) argument_parameters: Vec<(usize, usize)>,
    /// Solved method type arguments in formal order. Published on the call when every formal
    /// has one, matching an ordinary extension call.
    pub(super) type_arguments: Vec<Option<Ty>>,
    /// Complete call-site solution retained until the selected candidate commits its arguments.
    pub(super) bindings: crate::symbol_resolver::GSigBinds,
    /// Declaration-owned lower constraints gathered from the receiver, contexts, and arguments.
    /// The selected candidate normalizes them into intersection facts at commitment time.
    pub(super) lower_inputs: HashMap<String, Vec<Ty>>,
    /// Maps the source-visible parameter order back to the normalized declaration parameters.
    pub(super) parameter_indices: Vec<usize>,
}

#[derive(Clone, Copy)]
pub(super) struct MemberExtensionCall<'a> {
    pub(super) extension_receiver: Ty,
    pub(super) name: &'a str,
    pub(super) args: &'a [ExprId],
    pub(super) partial_arg_tys: &'a [Option<Ty>],
    pub(super) arg_names: Option<&'a [Option<String>]>,
    pub(super) explicit_type_args: &'a [Ty],
    pub(super) trailing_lambda: bool,
}
