//! Terminal diagnostics for rejected receiver-less callable candidates.

use super::super::*;

pub(in crate::resolve) fn report_inapplicable_callable_candidates(
    site: InapplicableTopLevelCall<'_>,
    candidates: Vec<crate::libraries::FunctionInfo>,
    checker: &mut Checker<'_>,
) -> bool {
    let InapplicableTopLevelCall {
        call,
        name,
        args,
        argument_names,
        trailing_lambda,
        mapping_error_reported,
        explicit_type_args,
    } = site;
    if checker.call_has_error_argument(args) {
        return true;
    }
    let inaccessible_private = candidates
        .iter()
        .any(|candidate| !checker.source_callable_visible(candidate));
    let candidates = candidates
        .into_iter()
        .filter(|candidate| checker.source_callable_visible(candidate))
        .collect::<Vec<_>>();
    if candidates.is_empty() && inaccessible_private {
        checker.diags.error(
            checker.call_callee_name_span(call),
            format!("cannot access '{name}': it is private in its file"),
        );
        return true;
    }
    crate::trace_compiler!(
        "resolve",
        "inapplicable callable {name}: candidates={} args={:?} names={argument_names:?}",
        candidates.len(),
        args
    );
    if let [candidate] = candidates.as_slice() {
        let context_count = candidate.context_count.min(candidate.callable.params.len());
        let value_signature = candidate.call_sig.suffix(context_count);
        let argument_parameters = call_argument_parameter_indices(
            args.len(),
            candidate
                .semantic_signature()
                .params
                .len()
                .saturating_sub(context_count),
            argument_names,
            trailing_lambda,
            &value_signature,
        );
        if checker.report_dnn_null_inference(
            call,
            candidate,
            args,
            argument_names,
            trailing_lambda,
            explicit_type_args.len(),
        ) {
            return true;
        }
        if checker.report_rejected_explicit_type_argument_bounds(
            call,
            candidate,
            !args.is_empty(),
            &explicit_type_args,
        ) {
            return true;
        }
        let mut generic_binding = None;
        let mut argument_contradiction = None;
        let semantic_params = if let Some(signature) = candidate.generic_sig.as_ref() {
            let mut bindings =
                crate::symbol_resolver::seeded_gsig_binds(signature, &explicit_type_args);
            if let Some(parameters) = &argument_parameters {
                let whole_arrays =
                    named_whole_array_varargs(parameters, argument_names, &value_signature);
                let source = checker.fed_source();
                let actuals = parameters
                    .iter()
                    .copied()
                    .zip(args.iter().copied())
                    .zip(whole_arrays)
                    .enumerate()
                    .map(|(source, ((parameter, argument), named_whole_array))| {
                        let actual = checker.expr_types[argument.0 as usize];
                        let whole_array =
                            named_whole_array || checker.file.is_spread_arg(args[source]);
                        (source, parameter + context_count, actual, whole_array)
                    })
                    .collect::<Vec<_>>();
                argument_contradiction = checker.argument_contradictions(
                    signature,
                    &actuals,
                    &explicit_type_args,
                    candidate.call_sig.vararg_index,
                    args,
                    Checker::generic_callable_header(name, candidate),
                );
                let inferred = crate::symbol_resolver::infer_generic_call_bindings_from_symbols(
                    &source,
                    signature,
                    actuals
                        .into_iter()
                        .map(|(_, parameter, actual, whole_array)| {
                            (parameter, actual, whole_array)
                        }),
                    candidate.call_sig.vararg_index,
                );
                crate::symbol_resolver::merge_generic_bindings_from(
                    Some(&source),
                    signature,
                    explicit_type_args.as_slice(),
                    &mut bindings,
                    inferred,
                );
                for (formal_index, formal) in signature.formals.iter().enumerate() {
                    if explicit_type_args.get(formal_index).is_some() {
                        continue;
                    }
                    let inferred_from_callable_reference =
                        parameters.iter().enumerate().any(|(source, parameter)| {
                            matches!(checker.file.expr(args[source]), Expr::CallableRef { .. })
                                && signature.params.get(parameter + context_count).is_some_and(
                                    |parameter| {
                                        ty_mentions_param(*parameter, std::slice::from_ref(formal))
                                    },
                                )
                        });
                    // `Nothing` is the constraint solver's bottom completion, not a proper
                    // inferred input type contributed by a callable reference. When the only
                    // occurrence of `T` is contravariant in `(T) -> R`, retaining that bottom
                    // would turn the real failure into an argument mismatch against
                    // `(Nothing) -> R`. Kotlin instead reports that `T` could not be inferred.
                    // A concrete reference input (or an explicit type argument) binds the
                    // formal before this diagnostic path.
                    let unresolved = bindings
                        .get(formal.as_str())
                        .is_none_or(|binding| binding.non_null() == Ty::Nothing);
                    if inferred_from_callable_reference && unresolved {
                        checker.diags.error(
                            checker.span(call),
                            format!(
                                "cannot infer type for type parameter '{}'. Specify it explicitly.",
                                crate::types::type_parameter_source_name(formal)
                            ),
                        );
                        return true;
                    }
                    if bindings.contains_key(formal.as_str()) {
                        continue;
                    }
                }
            }
            let params = signature
                .params
                .iter()
                .map(|parameter| crate::symbol_resolver::ty_subst(*parameter, &bindings))
                .collect();
            generic_binding = Some((signature, bindings));
            params
        } else {
            candidate.semantic_params().to_vec()
        };
        match map_call_sig_args_with_trailing(
            args,
            argument_names,
            semantic_params.len().saturating_sub(context_count),
            &value_signature,
            trailing_lambda,
        ) {
            Err(error) if !mapping_error_reported => {
                checker.report_callable_arg_mapping_error(
                    call,
                    args,
                    DiagnosticFunction {
                        name,
                        params: &candidate.callable.params,
                        param_names: &candidate.call_sig.param_names,
                        param_defaults: &candidate.call_sig.param_defaults,
                        required: candidate.call_sig.required,
                        vararg_index: candidate.call_sig.vararg_index,
                        context_count: candidate.context_count,
                        ret: candidate.callable.ret,
                        source_display: checker
                            .module_source_display(candidate, candidate.callable.ret),
                    },
                    error,
                );
                return true;
            }
            Ok(_) => {
                let before = checker.diags.diags.len();
                if let Some((signature, bindings)) = &generic_binding {
                    if checker.report_generic_argument_bounds(
                        generic_argument_bounds::GenericArgumentBounds {
                            call,
                            name,
                            args,
                            parameters: argument_parameters.as_deref(),
                            context_count,
                            explicit_type_args: &explicit_type_args,
                            signature,
                            bindings,
                            contradictions: argument_contradiction.take(),
                        },
                    ) {
                        return true;
                    }
                }
                for (source, &parameter) in argument_parameters.iter().flatten().enumerate() {
                    let parameter = parameter + context_count;
                    let Some(&declared) = semantic_params.get(parameter) else {
                        continue;
                    };
                    let argument = args[source];
                    let nominal_actual = checker.expr_types[argument.0 as usize];
                    let actual = checker
                        .callable_reference_types
                        .get(&argument)
                        .copied()
                        .filter(|_| declared.non_null().is_fun())
                        .unwrap_or(nominal_actual);
                    let vararg = candidate.call_sig.vararg_index == Some(parameter);
                    let named = argument_names
                        .and_then(|names| names.get(source))
                        .is_some_and(Option::is_some);
                    if vararg && (named || checker.file.is_spread_arg(argument)) {
                        checker.expect_whole_array_vararg_arg(None, argument, actual, declared);
                        continue;
                    }
                    let whole_array = vararg && checker.receiver_is_assignable(actual, declared);
                    let expected = if vararg && !whole_array {
                        declared.array_read_elem().unwrap_or(declared)
                    } else {
                        declared
                    };
                    crate::trace_compiler!(
                        "resolve",
                        "inapplicable callable {name}: argument={argument:?} parameter={parameter} expected={expected:?} actual={actual:?} vararg={vararg} whole_array={whole_array}"
                    );
                    let context = if generic_binding.is_some() {
                        "generic argument"
                    } else {
                        "argument"
                    };
                    if !candidate
                        .call_sig
                        .parameter_admits(parameter, expected, actual)
                        || checker.function_value_rejected(argument, expected, actual)
                    {
                        checker.report_assignability_error(
                            expected,
                            checker.suspend_converted_argument(expected, actual),
                            checker.span(argument),
                            context,
                        );
                    } else {
                        checker.expect_assignable(
                            expected,
                            actual,
                            checker.span(argument),
                            context,
                        );
                    }
                }
                return checker.diags.diags.len() != before;
            }
            Err(_) => {}
        }
        return false;
    }
    if candidates.is_empty() {
        return false;
    }
    let mut seen_keys = crate::fx_hash::HashSet::default();
    let legacy_symbols = checker.module.legacy_symbols();
    let conflict_keys = candidates
        .iter()
        .filter_map(|candidate| {
            let source = candidate.source_key?;
            legacy_symbols?
                .conflicting_top_level_key_by_source
                .get(&source)
                .filter(|key| seen_keys.insert((*key).clone()))
                .cloned()
        })
        .collect::<Vec<_>>();
    let mut mapping_errors = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let context_count = candidate.context_count.min(candidate.callable.params.len());
        let value_signature = candidate.call_sig.suffix(context_count);
        match map_call_sig_args_with_trailing(
            args,
            argument_names,
            candidate
                .semantic_signature()
                .params
                .len()
                .saturating_sub(context_count),
            &value_signature,
            trailing_lambda,
        ) {
            Ok(_) => {}
            Err(error) => mapping_errors.push((error, index)),
        }
    }
    let mut reported_mapping_error = false;
    let same_mapping_shape = candidates.first().is_some_and(|first| {
        candidates
            .iter()
            .skip(1)
            .all(|candidate| Checker::same_argument_mapping_shape(first, candidate))
    });
    if !mapping_error_reported && same_mapping_shape && mapping_errors.len() == candidates.len() {
        if let Some((error, candidate_index)) = take_unanimous_mapping_error(&mut mapping_errors) {
            let candidate = &candidates[candidate_index];
            checker.report_callable_arg_mapping_error(
                call,
                args,
                DiagnosticFunction {
                    name,
                    params: &candidate.callable.params,
                    param_names: &candidate.call_sig.param_names,
                    param_defaults: &candidate.call_sig.param_defaults,
                    required: candidate.call_sig.required,
                    vararg_index: candidate.call_sig.vararg_index,
                    context_count: candidate.context_count,
                    ret: candidate.callable.ret,
                    source_display: checker
                        .module_source_display(candidate, candidate.callable.ret),
                },
                error,
            );
            reported_mapping_error = true;
        }
    }

    let prefix = INAPPLICABLE_OVERLOAD_PREFIX;
    let mut displays: Vec<String> = Vec::with_capacity(MAX_OVERLOAD_DIAGNOSTIC_CANDIDATES);
    let mut display_bytes = 0usize;
    let mut seen = crate::fx_hash::HashSet::default();
    for key in conflict_keys {
        let Some(conflicts) = checker
            .module
            .legacy_symbols()
            .and_then(|symbols| symbols.conflicting_top_level_candidates.get(&key))
        else {
            continue;
        };
        let local = conflicts
            .by_file
            .get(&checker.file_index)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for candidate in conflicts
            .public
            .first()
            .into_iter()
            .chain(local.first())
            .chain(&conflicts.public)
            .chain(local)
        {
            if displays.len() >= MAX_OVERLOAD_DIAGNOSTIC_CANDIDATES {
                break;
            }
            if !seen.insert((candidate.file, candidate.declaration)) {
                continue;
            }
            let separator_bytes = if displays.is_empty() { 2 } else { 1 };
            if prefix
                .len()
                .saturating_add(display_bytes)
                .saturating_add(separator_bytes)
                .saturating_add(candidate.display.len())
                > MAX_OVERLOAD_DIAGNOSTIC_BYTES
            {
                break;
            }
            display_bytes += separator_bytes + candidate.display.len();
            displays.push(candidate.display.clone());
        }
    }
    if displays.is_empty() {
        for candidate in overload_diagnostics::visibility_diverse_candidate_indices(
            &candidates,
            checker.file_index,
            MAX_OVERLOAD_DIAGNOSTIC_CANDIDATES,
        )
        .into_iter()
        .map(|index| &candidates[index])
        {
            if displays.len() >= MAX_OVERLOAD_DIAGNOSTIC_CANDIDATES {
                break;
            }
            let display = checker
                .module_source_display(candidate, candidate.callable.ret)
                .unwrap_or_else(|| Checker::callable_candidate_display(name, candidate));
            let separator_bytes = if displays.is_empty() { 2 } else { 1 };
            if prefix
                .len()
                .saturating_add(display_bytes)
                .saturating_add(separator_bytes)
                .saturating_add(display.len())
                > MAX_OVERLOAD_DIAGNOSTIC_BYTES
            {
                break;
            }
            display_bytes += separator_bytes + display.len();
            displays.push(display);
        }
    }
    if displays.is_empty() {
        return reported_mapping_error;
    }
    if let Some(message) = explicit_bound_lambda_rejection(
        checker,
        name,
        &candidates,
        args,
        argument_names,
        trailing_lambda,
        &explicit_type_args,
    ) {
        checker
            .diags
            .error(checker.call_callee_name_span(call), message);
        return true;
    }
    displays.sort_unstable();
    let mut message = String::with_capacity(prefix.len() + display_bytes);
    message.push_str(prefix);
    for (index, display) in displays.iter().enumerate() {
        message.push_str(if index == 0 { "\n\n" } else { "\n" });
        message.push_str(display);
    }
    checker
        .diags
        .error(checker.call_callee_name_span(call), message);
    true
}

/// Both overloads' explicit type arguments are outside their bounds, so neither shapes the lambda.
/// kotlinc lists every candidate in declaration order with the mismatch that leaves the lambda
/// uninferred, not a sorted signature list.
fn explicit_bound_lambda_rejection(
    checker: &Checker<'_>,
    name: &str,
    candidates: &[crate::libraries::FunctionInfo],
    args: &[ExprId],
    argument_names: Option<&[Option<String>]>,
    trailing_lambda: bool,
    explicit_type_args: &[Ty],
) -> Option<String> {
    if explicit_type_args.is_empty()
        || explicit_type_args
            .iter()
            .any(|argument| argument.mentions_error() || argument.mentions_pending())
        || !args
            .iter()
            .any(|argument| matches!(checker.file.expr(*argument), Expr::Lambda { .. }))
    {
        return None;
    }
    if !candidates.iter().all(|candidate| {
        let Some(signature) = candidate.generic_sig.as_ref() else {
            return false;
        };
        signature.formals.len() == explicit_type_args.len()
            && !crate::symbol_resolver::generic_bindings_satisfy_bounds(
                signature,
                &crate::symbol_resolver::seeded_gsig_binds(signature, explicit_type_args),
                |actual, bound| checker.generic_bound_admits(actual, bound),
            )
    }) {
        return None;
    }
    let mut message = String::from(INAPPLICABLE_OVERLOAD_PREFIX);
    for candidate in candidates {
        let display = checker
            .module_source_display(candidate, candidate.callable.ret)
            .unwrap_or_else(|| Checker::callable_candidate_display(name, candidate));
        let reason = lambda_bound_rejection_reason(
            checker,
            candidate,
            args,
            argument_names,
            trailing_lambda,
        )
        .unwrap_or_else(|| "Inapplicable candidate.".to_string());
        message.push_str(&format!("\n\n{display}:\n  {reason}"));
    }
    Some(message)
}

fn lambda_bound_rejection_reason(
    checker: &Checker<'_>,
    candidate: &crate::libraries::FunctionInfo,
    args: &[ExprId],
    argument_names: Option<&[Option<String>]>,
    trailing_lambda: bool,
) -> Option<String> {
    let signature = candidate.semantic_signature();
    let context_count = candidate.context_count.min(signature.params.len());
    let argument_parameters = call_argument_parameter_indices(
        args.len(),
        signature.params.len().saturating_sub(context_count),
        argument_names,
        trailing_lambda,
        &candidate.call_sig.suffix(context_count),
    )?;
    let (source, parameter) =
        argument_parameters
            .into_iter()
            .enumerate()
            .find_map(|(source, parameter)| {
                matches!(checker.file.expr(args[source]), Expr::Lambda { .. })
                    .then_some((source, parameter + context_count))
            })?;
    let expected = signature.params.get(parameter).copied()?;
    let TyKind::Fun(function) = expected.non_null().kind() else {
        return None;
    };
    let Expr::Lambda { params, .. } = checker.file.expr(args[source]) else {
        return None;
    };
    let owner = Checker::generic_callable_header(&candidate.callable.name, candidate);
    let formals = signature.formals.as_slice();
    let value_start =
        function.context_count.min(function.params.len()) + usize::from(function.has_receiver);
    let value_params = &function.params[value_start.min(function.params.len())..];
    let lambda_arity = if params.is_empty() { 1 } else { params.len() };
    if params.is_empty() || params.len() == value_params.len() {
        let parameter_name = params.first().map(|name| name.as_str()).unwrap_or("it");
        let expected_parameter = value_params
            .first()
            .copied()
            .unwrap_or(Ty::obj("kotlin/Any"));
        return Some(format!(
            "Parameter type mismatch: actual type is '??? (Unknown type for value parameter {parameter_name})', but '{}' was expected.",
            render_bound_rejected_type(expected_parameter, &owner, formals)
        ));
    }
    let taken = lambda_arity.min(value_params.len());
    let actual = render_function_type(&value_params[..taken], function.ret, &owner, formals);
    let expected = render_function_type(value_params, function.ret, &owner, formals);
    Some(format!(
        "Argument type mismatch: actual type is '{actual}', but '{expected}' was expected."
    ))
}

fn render_function_type(
    params: &[Ty],
    ret: Ty,
    owner: &str,
    formals: &[crate::types::TyParamName],
) -> String {
    let params = params
        .iter()
        .map(|parameter| render_bound_rejected_type(*parameter, owner, formals))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "({params}) -> {}",
        render_bound_rejected_type(ret, owner, formals)
    )
}

fn render_bound_rejected_type(
    ty: Ty,
    owner: &str,
    formals: &[crate::types::TyParamName],
) -> String {
    match ty.non_null().kind() {
        TyKind::TyParam(name, _) | TyKind::Var(name, _)
            if formals.iter().any(|formal| formal.as_str() == name) =>
        {
            format!(
                "uninferred {} (of {owner})",
                crate::types::type_parameter_source_name(name.as_str())
            )
        }
        TyKind::Fun(signature) => {
            let start = signature.context_count.min(signature.params.len())
                + usize::from(signature.has_receiver);
            render_function_type(&signature.params[start..], signature.ret, owner, formals)
        }
        _ => ty.source_name(),
    }
}
