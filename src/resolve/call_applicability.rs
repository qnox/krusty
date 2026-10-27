//! Syntax-aware applicability shared by all semantic callable candidates.

use super::*;

/// The per-argument view of a call site used during overload scoring: the argument expressions,
/// whatever types are known so far, their labels, and whether the last one is a trailing lambda.
/// All four are indexed by the same argument position.
#[derive(Clone, Copy)]
pub(super) struct ArgSlots<'a> {
    pub(super) args: &'a [ExprId],
    pub(super) partial_arg_tys: &'a [Option<Ty>],
    pub(super) arg_names: Option<&'a [Option<String>]>,
    pub(super) trailing_lambda: bool,
}

/// Applicability facts shared by member and receiver-less calls. `rank` is deliberately independent
/// of declaration origin; `sam_signatures` preserves the exact adaptation selected for each lambda.
pub(super) struct CallCandidateScore {
    pub(super) rank: (usize, std::cmp::Reverse<usize>, bool),
    pub(super) sam_signatures: Vec<Option<crate::symbol_resolver::SamSignature>>,
    /// Every checked lambda body's result fits its parameter. Applicability never depends on
    /// it; only a refinable most-specific tie consults it.
    pub(super) lambda_result_fits: bool,
}

impl CallCandidateScore {
    pub(super) fn uses_sam_conversion(&self) -> bool {
        self.sam_signatures.iter().any(Option::is_some)
    }
}

/// Score one semantic call shape against syntax-aware partial arguments.
///
/// This is the sole applicability primitive for source/module member planning and receiver-less
/// SAM arbitration. It accepts only parameter and [`CallSig`] facts, so adding another declaration
/// provider cannot produce another scoring implementation. Lambda literal and spread provenance
/// remain explicit because those are properties of the call syntax, not of the provider.
pub(super) fn score(
    checker: &Checker<'_>,
    scope: &CheckerScope<'_>,
    params: &[Ty],
    call_sig: &CallSig,
    slots: ArgSlots<'_>,
) -> Option<CallCandidateScore> {
    let ArgSlots {
        args,
        partial_arg_tys,
        arg_names,
        trailing_lambda,
    } = slots;
    let sam_signatures = std::cell::RefCell::new(vec![None; args.len()]);
    let lambda_result_mismatch = std::cell::Cell::new(false);
    let record_sam = |source: usize, expected: Ty| {
        let signature = checker.semantic_sam_signature(expected)?;
        *sam_signatures.borrow_mut().get_mut(source)? = Some(signature);
        Some(())
    };
    // Candidate scoring receives the expression as well as its runtime type so every module
    // call shape (positional, named, defaulted, and vararg) consults the same literal adaptation
    // carried by `CallArgKind`.
    let score = |parameter: usize, expected: Ty, actual: Ty, argument: ExprId, source: usize| {
        let expected = checker.declared_function_semantic_type(expected);
        let semantic_actual = checker.argument_type_for_expected(scope, argument, actual, expected);
        let semantic_actual = checker
            .unit_coerced_lambda_type(argument, expected, semantic_actual)
            .unwrap_or(semantic_actual);
        let contextual_call_result = checker.call_result_can_bind_expected(argument, expected);
        // A collection literal is an expected-type-dependent expression. Its expectation-free
        // probe deliberately materializes the language fallback (`List<T>`), but that probe is
        // not applicability evidence against a candidate whose parameter supplies an array or
        // a classifier factory. Admit the candidate from the semantic target strategy here;
        // the selected-call seam below rechecks the literal and commits the exact `of`/factory
        // target. Treating the provisional `List<T>` as final rejects the enclosing callable
        // before its parameter can participate at all.
        let contextual_collection_literal =
            checker.file.collection_literal_calls.contains(&argument.0)
                && checker.collection_literal_target_available(expected);
        let contextual_name = checker
            .postponed_context_sensitive_name(scope, argument)
            .is_some_and(|name| checker.context_sensitive_name_matches(name, expected));
        let contextual_empty_reference_array = checker
            .postponed_empty_reference_array_factory(scope, argument)
            && expected.non_null().is_reference_array()
            && expected.non_null().array_elem().is_some();
        crate::trace_compiler!(
                "expected_call",
                "score argument={argument:?} parameter={parameter} expected={expected:?} actual={semantic_actual:?} exact={}",
                call_sig
                    .exact_params
                    .get(parameter)
                    .copied()
                    .unwrap_or(false),
            );
        // A nested declaration result may still be the signature engine's private `Pending`
        // marker while an outer postponed-call frame is collecting constraints. It provides no
        // overload preference, but it cannot make an otherwise shaped candidate inapplicable;
        // the declaration is demanded and finalized before checked FIR is constructed.
        if expected.mentions_pending() || semantic_actual.mentions_pending() {
            return Some(0);
        }
        if (checker.postponed_call_mentions(expected)
            || checker.postponed_call_mentions(semantic_actual))
            && !checker.postponed_argument_constraint_is_satisfiable(expected, semantic_actual)
        {
            return None;
        }
        // An unannotated lambda's expectation-free `Any` parameters are placeholders, not
        // fixed input types. Once explicit type arguments or earlier constraints expose a
        // concrete function slot, admit the candidate by the lambda's source arity and let the
        // selected-call commit recheck its body in that exact receiver/context/value scope.
        if checker.lambda_parameters_are_contextual(argument) {
            if let TyKind::Fun(expected_function) = expected.non_null().kind() {
                if checker.contextual_lambda_accepts_function_shape(argument, expected_function) {
                    // Kotlin decides applicability before analyzing the lambda body, so the
                    // checked result only marks this candidate for a refinement the selection
                    // may apply to a most-specific tie (`flatMap { list }` fits
                    // `(T) -> Iterable<R>`, not `(T) -> Sequence<R>`). A bare result variable
                    // remains postponed; a constructed result such as `Sequence<R>` must already
                    // have the right semantic outer type.
                    let result_admitted = match semantic_actual.non_null().kind() {
                        TyKind::Fun(actual_function) => {
                            let fits = expected_function.ret == Ty::Unit
                                || actual_function.ret.mentions_error()
                                || checker.receiver_is_assignable(
                                    actual_function.ret,
                                    expected_function.ret,
                                )
                                || expected_function.ret.non_null().is_ty_param();
                            lambda_result_mismatch.set(lambda_result_mismatch.get() || !fits);
                            true
                        }
                        TyKind::Error | TyKind::Pending => true,
                        _ => false,
                    };
                    if result_admitted {
                        return Some(2);
                    }
                }
            }
        }
        if contextual_collection_literal {
            return Some(2);
        }
        if contextual_name {
            return Some(2);
        }
        if contextual_empty_reference_array {
            return Some(2);
        }
        // A lambda/function-shaped value is not nominally assignable to its SAM classifier;
        // the conversion is precisely what makes the argument applicable. Evaluate that
        // semantic conversion before the ordinary nominal rejection below. Its score remains
        // lower than a direct function/`Any` fit, preserving Kotlin's overload preference.
        // A value already assignable to this exact applied interface is an ordinary argument:
        // recording a conversion here would rank it below that direct fit and could select
        // the adapted overload.
        let sam_conversion = if checker.sam_argument_already_implements(argument, actual, expected)
        {
            None
        } else {
            let argument_kind = call_arg_kind(checker.file, argument, actual);
            let probe = if argument_kind.is_lambda_literal() {
                checker.lambda_probe_ty(scope, argument).unwrap_or(actual)
            } else {
                actual
            };
            checker
                .sam_argument_fits(scope, expected, probe, argument)
                .then(|| record_sam(source, expected))
                .flatten()
        };
        if sam_conversion.is_some() {
            return Some(0);
        }
        // A lambda literal with declared parameter types still postpones its body, so only its
        // inputs decide applicability; its result is consulted as for an untyped lambda above.
        let semantic_actual = match (
            checker.file.expr(argument),
            semantic_actual.non_null().kind(),
            expected.non_null().kind(),
        ) {
            (Expr::Lambda { .. }, TyKind::Fun(actual), TyKind::Fun(expected)) => {
                let fits = checker.receiver_is_assignable(actual.ret, expected.ret)
                    || expected.ret == Ty::Unit;
                lambda_result_mismatch.set(lambda_result_mismatch.get() || !fits);
                Ty::fun_with_shape(
                    actual.params.clone(),
                    expected.ret,
                    actual.context_count,
                    actual.has_receiver,
                    actual.suspend,
                )
            }
            _ => semantic_actual,
        };
        if !call_sig.parameter_admits(parameter, expected, semantic_actual)
            && !contextual_call_result
        {
            return None;
        }
        checker
            .argument_uses_context_only_result(scope, argument)
            .then_some(1)
            .or_else(|| checker.member_argument_score(expected, semantic_actual))
            .or_else(|| contextual_call_result.then_some(0))
            .or_else(|| {
                checker
                    .unbound_call_result_signature(argument)
                    .is_some_and(|generic| {
                        crate::symbol_resolver::infer_generic_return_bindings(
                            &generic,
                            expected,
                            |actual, bound| checker.receiver_is_assignable(actual, bound),
                        )
                        .is_some()
                    })
                    .then_some(0)
            })
            .or_else(|| {
                expected
                    .non_null()
                    .is_fun()
                    .then(|| {
                        checker
                            .generic_function_constructor_arg_fits(expected, semantic_actual)
                            .then_some(1)
                    })
                    .flatten()
            })
            .or_else(|| {
                call_arg_kind(checker.file, argument, semantic_actual)
                    .adapts_integer_literal_to(expected)
                    .then_some(1)
            })
            .or_else(|| {
                checker
                    .implicit_integer_coercion_applies(
                        argument,
                        expected,
                        call_sig
                            .implicit_integer_coercion
                            .get(parameter)
                            .copied()
                            .unwrap_or(false),
                    )
                    .then_some(1)
            })
    };
    // A not-yet-checked lambda slot (a plan's partial pass leaves it untyped): a function-type
    // or erased-top parameter binds the lambda without conversion; a JAVA SAM parameter only
    // through the conversion (ranked lower, and only when the probe arity fits the SAM method);
    // anything else cannot take the lambda at all.
    let lambda_slot = |expected: Ty, argument: ExprId, source: usize| -> Option<usize> {
        let expected = checker.declared_function_semantic_type(expected);
        if !matches!(checker.file.expr(argument), Expr::Lambda { .. }) {
            // A postponed callable reference is candidate evidence once the candidate supplies
            // an exact function shape. Reject an outer overload whose parameter cannot select
            // the reference; otherwise two function-typed overloads remain falsely tied.
            return match checker.file.expr(argument) {
                Expr::CallableRef {
                    receiver: Some(receiver),
                    name,
                } if expected.non_null().is_fun() => {
                    let TyKind::Fun(expected_function) = expected.non_null().kind() else {
                        unreachable!("guarded function expectation")
                    };
                    match checker.receiver_qualified_callable_reference_adapts_to(
                        scope,
                        *receiver,
                        name,
                        expected_function,
                    ) {
                        Some(true) => Some(2),
                        Some(false) if expected.mentions_ty_param() => Some(0),
                        Some(false) => None,
                        None => Some(0),
                    }
                }
                _ => Some(0),
            };
        }
        if let TyKind::Fun(function) = expected.non_null().kind() {
            return checker
                .contextual_lambda_accepts_function_shape(argument, function)
                .then_some(2);
        }
        if expected.is_erased_top() {
            return Some(2);
        }
        let probe = checker
            .lambda_probe_ty(scope, argument)
            .unwrap_or(Ty::Error);
        if checker.sam_argument_fits(scope, expected, probe, argument) {
            record_sam(source, expected)?;
            Some(1)
        } else {
            None
        }
    };
    if call_sig.vararg {
        // A vararg is not necessarily the last declaration slot (`vararg xs, block`) and one
        // slot may represent several source expressions. Map every SOURCE index through the
        // common call-shape mapper, then score it against the declared vararg index. The former
        // `split_last` path made the trailing block the array and the generic slot path saw only
        // the first packed element, so valid non-last varargs depended on which syntax selected
        // the overload.
        let slots = map_call_sig_args_with_trailing(
            args,
            arg_names,
            params.len(),
            call_sig,
            trailing_lambda,
        )
        .ok()?;
        let parameters = call_argument_parameter_indices(
            args.len(),
            params.len(),
            arg_names,
            trailing_lambda,
            call_sig,
        )?;
        let vararg = call_sig.vararg_index?;
        let mut type_score = 0;
        for (source, (&argument, parameter)) in args.iter().zip(parameters).enumerate() {
            let Some(actual) = partial_arg_tys.get(source).copied().flatten() else {
                continue;
            };
            let declared = *params.get(parameter)?;
            let spread = checker.file.is_spread_arg(argument);
            if spread && parameter != vararg {
                return None;
            }
            let named = arg_names
                .and_then(|names| names.get(source))
                .is_some_and(Option::is_some);
            let (expected, actual) = if parameter == vararg {
                let element = declared.array_read_elem()?;
                let contributed = if spread || named {
                    actual.non_null().array_read_elem().unwrap_or(actual)
                } else {
                    actual
                };
                // Preserve the ordinary element-based vararg comparison. The sole exception is
                // an erased provisional array produced by a nested generic call whose element
                // has no generic structure of its own: only the full array expectation can type
                // `arrayOf("K")` as `Array<String>`. A structured element such as `Sink<Any>` is
                // already useful inference evidence and must not be widened contextually.
                let contextual_array = contributed.is_erased_top()
                    && element.type_args().is_empty()
                    && actual.non_null().array_elem().is_some()
                    && checker
                        .unbound_call_result_signature(argument)
                        .is_some_and(|generic| {
                            crate::symbol_resolver::infer_generic_return_bindings(
                                &generic,
                                declared,
                                |actual, bound| checker.receiver_is_assignable(actual, bound),
                            )
                            .is_some()
                        });
                if contextual_array {
                    (declared, actual)
                } else {
                    (element, contributed)
                }
            } else {
                (declared, actual)
            };
            type_score += score(parameter, expected, actual, argument, source)?;
        }
        return Some(CallCandidateScore {
            rank: (
                type_score,
                std::cmp::Reverse(slots.iter().filter(|slot| slot.is_none()).count()),
                false,
            ),
            sam_signatures: sam_signatures.into_inner(),
            lambda_result_fits: !lambda_result_mismatch.get(),
        });
    }
    if arg_names.is_none() && !trailing_lambda {
        if args
            .iter()
            .any(|argument| checker.file.is_spread_arg(*argument))
            || args.len() > params.len()
            || (args.len() < params.len()
                && (args.len()..params.len()).any(|index| !call_sig.param_has_default(index)))
        {
            return None;
        }
        let mut type_score = 0;
        for (index, (expected, actual)) in params.iter().zip(partial_arg_tys).enumerate() {
            match actual {
                Some(actual) => type_score += score(index, *expected, *actual, args[index], index)?,
                // A plan's untyped slot: fit the lambda by its probe so the plan types it
                // against the RIGHT candidate's parameter, not an arbitrary tied one.
                None => type_score += lambda_slot(*expected, args[index], index)?,
            }
        }
        return Some(CallCandidateScore {
            rank: (
                type_score,
                std::cmp::Reverse(params.len().saturating_sub(args.len())),
                true,
            ),
            sam_signatures: sam_signatures.into_inner(),
            lambda_result_fits: !lambda_result_mismatch.get(),
        });
    }
    let slots =
        map_call_sig_args_with_trailing(args, arg_names, params.len(), call_sig, trailing_lambda)
            .ok()?;
    let mut type_score = 0;
    for (parameter_index, argument) in slots.iter().enumerate() {
        let Some(argument) = argument else {
            continue;
        };
        let source_index = args.iter().position(|candidate| candidate == argument)?;
        let parameter = params.get(parameter_index)?;
        let is_vararg_slot = call_sig.vararg && Some(parameter_index) == call_sig.vararg_index;
        let spread = checker.file.is_spread_arg(*argument);
        if spread && !is_vararg_slot {
            return None;
        }
        let expected = if is_vararg_slot && !spread {
            parameter.array_read_elem().unwrap_or(Ty::Error)
        } else {
            *parameter
        };
        match partial_arg_tys.get(source_index).copied().flatten() {
            Some(actual) => {
                type_score += score(parameter_index, expected, actual, *argument, source_index)?
            }
            None => type_score += lambda_slot(expected, *argument, source_index)?,
        }
    }
    Some(CallCandidateScore {
        rank: (
            type_score,
            std::cmp::Reverse(slots.iter().filter(|slot| slot.is_none()).count()),
            !call_sig.vararg,
        ),
        sam_signatures: sam_signatures.into_inner(),
        lambda_result_fits: !lambda_result_mismatch.get(),
    })
}

/// Under `+EagerLambdaAnalysis` a lambda's analyzed result discriminates the applicable
/// candidates before most-specific selection, as kotlinc's `EagerLambdaResolution` does: the
/// candidates the result does not fit are dropped while one still fits. Without the feature the
/// result takes part only in the specification's refinement of a tie.
pub(super) fn retain_eager_lambda_fits<T>(
    file: &crate::ast::File,
    applicable: &mut Vec<T>,
    fits: impl Fn(&T) -> bool,
) {
    if file.eager_lambda_analysis && applicable.iter().any(&fits) {
        applicable.retain(fits);
    }
}

/// Kotlin's lambda-return refinement of a most-specific tie. It applies only when a tied
/// candidate is `@OverloadResolutionByLambdaReturnType`, the call has exactly one lambda literal,
/// and every tied parameter for it is a function type with the same inputs; the candidates whose
/// checked lambda result does not fit are then dropped, and the refined set prefers unannotated
/// candidates. Every other tie is left for the ordinary ambiguity.
pub(super) fn refine_tie_by_lambda_result<T>(
    file: &crate::ast::File,
    args: &[ExprId],
    tied: &mut Vec<T>,
    candidate: impl Fn(&T) -> (&crate::libraries::FunctionInfo, &[Ty], bool),
) {
    let refines = |function: &crate::libraries::FunctionInfo| {
        function
            .callable
            .annotations
            .contains(&crate::types::type_name(
                "kotlin/OverloadResolutionByLambdaReturnType",
            ))
    };
    let mut lambdas = args
        .iter()
        .enumerate()
        .filter(|(_, &argument)| matches!(file.expr(argument), Expr::Lambda { .. }))
        .map(|(position, _)| position);
    let (Some(lambda), None) = (lambdas.next(), lambdas.next()) else {
        return;
    };
    let inputs = |tie: &T| match candidate(tie).1.get(lambda)?.non_null().kind() {
        TyKind::Fun(function) => Some((
            function.params.clone(),
            function.context_count,
            function.has_receiver,
            function.suspend,
        )),
        _ => None,
    };
    let Some(first) = tied.first().and_then(inputs) else {
        return;
    };
    if tied.len() < 2
        || !tied.iter().any(|tie| refines(candidate(tie).0))
        || !tied.iter().all(|tie| inputs(tie).as_ref() == Some(&first))
        || !tied.iter().any(|tie| candidate(tie).2)
    {
        return;
    }
    tied.retain(|tie| candidate(tie).2);
    if tied.iter().any(|tie| !refines(candidate(tie).0)) {
        tied.retain(|tie| !refines(candidate(tie).0));
    }
}
