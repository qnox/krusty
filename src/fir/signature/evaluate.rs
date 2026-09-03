//! Structural evaluation of compact signature expressions through the ordinary resolver adapter.

use super::*;

/// The sole structural evaluator for [`SigExpr`]. It knows only how graph nodes compose; every
/// semantic decision is delegated to [`SignatureSemantics`].
pub struct ResolverBackedSignatureEvaluator<'a, S> {
    semantics: &'a S,
}

impl<'a, S> ResolverBackedSignatureEvaluator<'a, S> {
    pub const fn new(semantics: &'a S) -> Self {
        Self { semantics }
    }
}

impl<S: SignatureSemantics> SignatureConstraintEvaluator
    for ResolverBackedSignatureEvaluator<'_, S>
{
    fn evaluate(
        &self,
        declaration: DeclarationId,
        result: SigExprId,
        graph: &SignatureGraph,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedSignature, DiagnosticId> {
        fn argument_probe<'a, S: SignatureSemantics>(
            semantics: &S,
            argument: &'a SigCallArgument,
            graph: &'a SignatureGraph,
            demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
            memo: &mut HashMap<SigExprId, ResolvedTy>,
            computing: &mut std::collections::HashSet<SigExprId>,
        ) -> Result<SigCallArgumentProbe<'a>, DiagnosticId> {
            let name = argument.name.map(|name| {
                graph
                    .name(name)
                    .expect("a call-argument name must belong to its graph")
            });
            if let Some(SigExpr::ContextualFunction {
                parameters,
                implicit_it,
                ..
            }) = graph.expr(argument.value)
            {
                return Ok(SigCallArgumentProbe::PostponedLambda {
                    parameter_count: parameters.len,
                    implicit_it,
                    name,
                    spread: argument.spread,
                });
            }
            if matches!(
                graph.expr(argument.value),
                Some(SigExpr::CallableReference(_) | SigExpr::BoundCallableReference { .. })
            ) {
                return Ok(SigCallArgumentProbe::PostponedCallableReference {
                    name,
                    spread: argument.spread,
                });
            }
            let ty =
                evaluate_expression(semantics, argument.value, graph, demand, memo, computing)?;
            Ok(SigCallArgumentProbe::Typed(ResolvedSigCallArgument {
                ty,
                name,
                spread: argument.spread,
                integer_literal: argument.integer_literal,
                lambda: argument.lambda,
                contextual_call: matches!(graph.expr(argument.value), Some(SigExpr::Call { .. })),
            }))
        }

        fn materialize_argument<'a, S: SignatureSemantics>(
            semantics: &S,
            argument: &'a SigCallArgument,
            expectation: Option<ResolvedTy>,
            graph: &'a SignatureGraph,
            demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
            memo: &mut HashMap<SigExprId, ResolvedTy>,
            computing: &mut std::collections::HashSet<SigExprId>,
        ) -> Result<ResolvedSigCallArgument<'a>, DiagnosticId> {
            let name = argument.name.map(|name| {
                graph
                    .name(name)
                    .expect("a call-argument name must belong to its graph")
            });
            if matches!(
                graph.expr(argument.value),
                Some(SigExpr::CallableReference(_) | SigExpr::BoundCallableReference { .. })
            ) {
                let ty = evaluate_callable_reference(
                    semantics,
                    argument.value,
                    expectation,
                    graph,
                    demand,
                    memo,
                    computing,
                )?;
                return Ok(ResolvedSigCallArgument {
                    ty,
                    name,
                    spread: argument.spread,
                    integer_literal: argument.integer_literal,
                    lambda: argument.lambda,
                    contextual_call: false,
                });
            }
            let Some(SigExpr::ContextualFunction {
                parameters,
                result,
                scope,
                implicit_it,
                suspend,
            }) = graph.expr(argument.value)
            else {
                let ty = match expectation.and_then(|expected| {
                    evaluate_expression_with_expected(
                        semantics,
                        argument.value,
                        expected,
                        graph,
                        demand,
                        memo,
                        computing,
                    )
                }) {
                    Some(result) => result?,
                    None => evaluate_expression(
                        semantics,
                        argument.value,
                        graph,
                        demand,
                        memo,
                        computing,
                    )?,
                };
                return Ok(ResolvedSigCallArgument {
                    ty,
                    name,
                    spread: argument.spread,
                    integer_literal: argument.integer_literal,
                    lambda: argument.lambda,
                    contextual_call: false,
                });
            };
            let contextual_declaration = graph
                .scope(scope)
                .expect("a contextual function must retain its declaration scope")
                .owner;
            let Some(expectation) = expectation else {
                // A lambda passed to a non-functional parameter (for example `Any`) has no
                // contextual function shape. Kotlin still gives an arrow-less, parameter-less
                // literal its natural `() -> R` type. The placeholder for a possible implicit
                // `it` is deliberately omitted here; it only becomes a parameter when a
                // functional expectation supplies that parameter.
                if implicit_it {
                    let result =
                        evaluate_expression(semantics, result, graph, demand, memo, computing)?;
                    let ty = semantics.make_contextual_function_type(
                        contextual_declaration,
                        &[],
                        result,
                        0,
                        false,
                        suspend,
                    )?;
                    return Ok(ResolvedSigCallArgument {
                        ty,
                        name,
                        spread: argument.spread,
                        integer_literal: argument.integer_literal,
                        lambda: argument.lambda,
                        contextual_call: false,
                    });
                }
                // A lambda whose value parameters all carry written types also has a natural
                // function type without an expectation. It was retained as contextual because a
                // selected callable may still contribute context/receiver inputs; if selection
                // supplies none, evaluate exactly the written inputs and ordinary body result.
                let mut resolved_parameters = Vec::new();
                for parameter in graph.operands(parameters).iter().copied() {
                    if matches!(graph.expr(parameter), Some(SigExpr::ContextualParameter(_))) {
                        return Err(semantics.missing_signature_diagnostic(contextual_declaration));
                    }
                    resolved_parameters.push(evaluate_expression(
                        semantics, parameter, graph, demand, memo, computing,
                    )?);
                }
                let result =
                    evaluate_expression(semantics, result, graph, demand, memo, computing)?;
                let ty = semantics.make_contextual_function_type(
                    contextual_declaration,
                    &resolved_parameters,
                    result,
                    0,
                    false,
                    suspend,
                )?;
                return Ok(ResolvedSigCallArgument {
                    ty,
                    name,
                    spread: argument.spread,
                    integer_literal: argument.integer_literal,
                    lambda: argument.lambda,
                    contextual_call: false,
                });
            };
            let Ty::Fun(expected) = expectation.get().non_null() else {
                return Err(semantics.missing_signature_diagnostic(contextual_declaration));
            };
            let value_start = expected.context_count.min(expected.params.len())
                + usize::from(expected.has_receiver);
            let expected_values = &expected.params[value_start.min(expected.params.len())..];
            if (implicit_it && expected_values.len() > 1)
                || (!implicit_it && expected_values.len() != parameters.len as usize)
            {
                return Err(semantics.missing_signature_diagnostic(contextual_declaration));
            }
            let mut actual_values = Vec::new();
            for (index, parameter) in graph.operands(parameters).iter().copied().enumerate() {
                let expected_value = expected_values.get(index).copied();
                let actual = match graph.expr(parameter) {
                    Some(SigExpr::ContextualParameter(declaration)) => {
                        let Some(expected_value) = expected_value else {
                            continue;
                        };
                        let expected_value = ResolvedTy::new(expected_value)
                            .map_err(|_| semantics.missing_signature_diagnostic(declaration))?;
                        memo.insert(parameter, expected_value);
                        expected_value
                    }
                    _ => evaluate_expression(semantics, parameter, graph, demand, memo, computing)?,
                };
                actual_values.push(actual);
            }
            let contextual_receiver = expected
                .has_receiver
                .then(|| expected.params.get(expected.context_count).copied())
                .flatten()
                .map(ResolvedTy::new)
                .transpose()
                .map_err(|_| semantics.missing_signature_diagnostic(contextual_declaration))?;
            let scoped_inputs = expected
                .params
                .iter()
                .copied()
                .map(ResolvedTy::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| semantics.missing_signature_diagnostic(contextual_declaration))?;
            let contextual_count = expected.context_count.min(scoped_inputs.len());
            let contextual_receivers = &scoped_inputs[..contextual_count];
            semantics.enter_contextual_function(
                contextual_declaration,
                &scoped_inputs,
                contextual_receivers,
                contextual_receiver,
            );
            let evaluated_result =
                evaluate_expression(semantics, result, graph, demand, memo, computing);
            semantics.exit_contextual_function(
                contextual_declaration,
                contextual_count + usize::from(contextual_receiver.is_some()),
            );
            let result = evaluated_result?;
            let expected_result = ResolvedTy::new(expected.ret)
                .map_err(|_| semantics.missing_signature_diagnostic(contextual_declaration))?;
            let result = semantics.contextual_function_result(
                contextual_declaration,
                result,
                expected_result,
            )?;
            let mut complete_parameters = expected.params[..value_start].to_vec();
            complete_parameters.extend(actual_values.iter().map(|parameter| parameter.get()));
            let complete_parameters = complete_parameters
                .into_iter()
                .map(ResolvedTy::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| semantics.missing_signature_diagnostic(DeclarationId::from_raw(0)))?;
            let ty = semantics.make_contextual_function_type(
                contextual_declaration,
                &complete_parameters,
                result,
                expected.context_count as u32,
                expected.has_receiver,
                suspend || expected.suspend,
            )?;
            Ok(ResolvedSigCallArgument {
                ty,
                name,
                spread: argument.spread,
                integer_literal: argument.integer_literal,
                lambda: argument.lambda,
                contextual_call: false,
            })
        }

        fn evaluate_callable_reference<S: SignatureSemantics>(
            semantics: &S,
            expression: SigExprId,
            expected: Option<ResolvedTy>,
            graph: &SignatureGraph,
            demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
            memo: &mut HashMap<SigExprId, ResolvedTy>,
            computing: &mut std::collections::HashSet<SigExprId>,
        ) -> Result<ResolvedTy, DiagnosticId> {
            match graph.expr(expression) {
                Some(SigExpr::CallableReference(target)) => {
                    let selection = graph
                        .callable_selection(target)
                        .expect("a deferred callable-reference selection must belong to its graph");
                    let scope = graph
                        .scope(selection.scope)
                        .expect("a deferred callable-reference scope must belong to its graph");
                    let spelling = graph
                        .name(selection.spelling)
                        .expect("a deferred callable-reference spelling must belong to its graph");
                    semantics.select_callable_reference(
                        scope,
                        spelling,
                        selection.origin,
                        expected,
                        demand,
                    )
                }
                Some(SigExpr::BoundCallableReference {
                    receiver,
                    classifier,
                    scope: reference_scope,
                    root,
                    target,
                }) => {
                    let use_value = match (classifier, root) {
                        (Some(_), Some(root)) => {
                            let scope = graph.scope(reference_scope).expect(
                                "a qualified callable-reference scope must belong to its graph",
                            );
                            let root = graph.name(root).expect(
                                "a qualified callable-reference root must belong to its graph",
                            );
                            let selection = graph.callable_selection(target).expect(
                                "a deferred bound callable-reference selection must belong to its graph",
                            );
                            let target_spelling = graph.name(selection.spelling).expect(
                                "a deferred bound callable-reference spelling must belong to its graph",
                            );
                            semantics.callable_reference_receiver_is_value(
                                scope,
                                root,
                                target_spelling,
                            )?
                        }
                        (None, None) => true,
                        (Some(_), None) | (None, Some(_)) => unreachable!(
                            "callable-reference classifier and root must travel together"
                        ),
                    };
                    let (receiver, use_value) = if use_value {
                        (
                            evaluate_expression(
                                semantics, receiver, graph, demand, memo, computing,
                            )?,
                            true,
                        )
                    } else {
                        let candidate = classifier.expect("a classifier candidate must exist");
                        match evaluate_expression(
                            semantics, candidate, graph, demand, memo, computing,
                        ) {
                            Ok(resolved) => (resolved, false),
                            Err(_) => (
                                evaluate_expression(
                                    semantics, receiver, graph, demand, memo, computing,
                                )?,
                                true,
                            ),
                        }
                    };
                    let selection = graph.callable_selection(target).expect(
                        "a deferred bound callable-reference selection must belong to its graph",
                    );
                    let scope = graph.scope(selection.scope).expect(
                        "a deferred bound callable-reference scope must belong to its graph",
                    );
                    let spelling = graph.name(selection.spelling).expect(
                        "a deferred bound callable-reference spelling must belong to its graph",
                    );
                    semantics.select_bound_callable_reference(
                        scope,
                        spelling,
                        selection.origin,
                        receiver,
                        !use_value,
                        expected,
                        demand,
                    )
                }
                _ => Err(semantics.missing_signature_diagnostic(DeclarationId::from_raw(0))),
            }
        }

        fn evaluate_call<S: SignatureSemantics>(
            semantics: &S,
            target: DeferredCallableSelectionId,
            arguments: CallArgumentRange,
            forced_expected: Option<ResolvedTy>,
            graph: &SignatureGraph,
            demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
            memo: &mut HashMap<SigExprId, ResolvedTy>,
            computing: &mut std::collections::HashSet<SigExprId>,
        ) -> Result<ResolvedTy, DiagnosticId> {
            let selection = graph
                .callable_selection(target)
                .expect("a deferred callable selection must belong to its graph");
            let scope = graph
                .scope(selection.scope)
                .expect("a deferred callable scope must belong to its graph");
            let spelling = graph
                .name(selection.spelling)
                .expect("a deferred callable spelling must belong to its graph");
            let mut resolved_type_arguments = Vec::new();
            for argument in graph.operands(selection.type_arguments).iter().copied() {
                resolved_type_arguments.push(evaluate_expression(
                    semantics, argument, graph, demand, memo, computing,
                )?);
            }
            let expected = match (forced_expected, selection.expected) {
                (Some(expected), _) => Some(expected),
                (None, Some(expected)) => Some(evaluate_expression(
                    semantics, expected, graph, demand, memo, computing,
                )?),
                (None, None) => None,
            };
            let call_arguments = graph.call_arguments(arguments);
            let mut probes = call_arguments
                .iter()
                .map(|argument| argument_probe(semantics, argument, graph, demand, memo, computing))
                .collect::<Result<Vec<_>, _>>()?;

            // Postponed arguments in one call form a constraint system, not an evaluation-order
            // dependency. A later lambda whose input is already concrete can determine a type
            // variable needed by an earlier lambda (`toRoot = { ... }, toDomain = { ... }`).
            // Materialize concrete-input postponed arguments first and ask the selected callable
            // declaration to specialize the remaining expectations after each contribution. The
            // final argument vector still retains source order; this only orders temporary
            // signature constraints.
            let mut materialized = vec![None; call_arguments.len()];
            while probes.iter().any(|probe| {
                matches!(
                    probe,
                    SigCallArgumentProbe::PostponedLambda { .. }
                        | SigCallArgumentProbe::PostponedCallableReference { .. }
                )
            }) {
                let expectations = semantics.call_argument_expectations(
                    scope,
                    spelling,
                    selection.origin,
                    &probes,
                    &resolved_type_arguments,
                    selection.trailing_lambda,
                    expected,
                    demand,
                )?;
                let postponed = probes
                    .iter()
                    .enumerate()
                    .filter(|(_, probe)| {
                        matches!(
                            probe,
                            SigCallArgumentProbe::PostponedLambda { .. }
                                | SigCallArgumentProbe::PostponedCallableReference { .. }
                        )
                    })
                    .collect::<Vec<_>>();
                let ready = |index: usize| {
                    let Some(expected) = expectations.get(index).copied().flatten() else {
                        return false;
                    };
                    let Ty::Fun(signature) = expected.get().non_null() else {
                        return false;
                    };
                    signature.params.iter().all(|parameter| {
                        !parameter.mentions_ty_param()
                            && !parameter.mentions_pending()
                            && !parameter.mentions_error()
                    })
                };
                let index = postponed
                    .iter()
                    .find_map(|(index, _)| ready(*index).then_some(*index))
                    .unwrap_or(postponed[0].0);
                let resolved = materialize_argument(
                    semantics,
                    &call_arguments[index],
                    expectations.get(index).copied().flatten(),
                    graph,
                    demand,
                    memo,
                    computing,
                )?;
                probes[index] = SigCallArgumentProbe::Typed(resolved);
                materialized[index] = Some(resolved);
            }

            let expectations = semantics.call_argument_expectations(
                scope,
                spelling,
                selection.origin,
                &probes,
                &resolved_type_arguments,
                selection.trailing_lambda,
                expected,
                demand,
            )?;
            let mut resolved_arguments = Vec::new();
            for (index, argument) in call_arguments.iter().enumerate() {
                resolved_arguments.push(match materialized[index] {
                    Some(argument) => argument,
                    None => materialize_argument(
                        semantics,
                        argument,
                        expectations.get(index).copied().flatten(),
                        graph,
                        demand,
                        memo,
                        computing,
                    )?,
                });
            }
            semantics.select_call(
                scope,
                spelling,
                selection.origin,
                &resolved_arguments,
                &resolved_type_arguments,
                selection.trailing_lambda,
                expected,
                demand,
            )
        }

        fn evaluate_expression_with_expected<S: SignatureSemantics>(
            semantics: &S,
            expression: SigExprId,
            expected: ResolvedTy,
            graph: &SignatureGraph,
            demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
            memo: &mut HashMap<SigExprId, ResolvedTy>,
            computing: &mut std::collections::HashSet<SigExprId>,
        ) -> Option<Result<ResolvedTy, DiagnosticId>> {
            match graph.expr(expression)? {
                SigExpr::Call { target, arguments } => Some(evaluate_call(
                    semantics,
                    target,
                    arguments,
                    Some(expected),
                    graph,
                    demand,
                    memo,
                    computing,
                )),
                SigExpr::NonNullable(base) => evaluate_expression_with_expected(
                    semantics, base, expected, graph, demand, memo, computing,
                )
                .map(|result| result.and_then(|result| semantics.make_non_nullable(result))),
                SigExpr::Nullable(base) => evaluate_expression_with_expected(
                    semantics, base, expected, graph, demand, memo, computing,
                )
                .map(|result| result.and_then(|result| semantics.make_nullable(result))),
                SigExpr::Sequence { effects, result } => Some((|| {
                    for effect in graph.operands(effects).iter().copied() {
                        evaluate_expression(semantics, effect, graph, demand, memo, computing)?;
                    }
                    evaluate_expression_with_expected(
                        semantics, result, expected, graph, demand, memo, computing,
                    )
                    .unwrap_or_else(|| {
                        evaluate_expression(semantics, result, graph, demand, memo, computing)
                    })
                })()),
                SigExpr::ScopedReceiver {
                    receiver,
                    result,
                    scope,
                } => Some((|| {
                    let receiver =
                        evaluate_expression(semantics, receiver, graph, demand, memo, computing)?;
                    let scope = graph
                        .scope(scope)
                        .expect("a scoped receiver must retain its declaration scope");
                    semantics.enter_scoped_receiver(scope.owner, receiver);
                    let evaluated = evaluate_expression_with_expected(
                        semantics, result, expected, graph, demand, memo, computing,
                    )
                    .unwrap_or_else(|| {
                        evaluate_expression(semantics, result, graph, demand, memo, computing)
                    });
                    semantics.exit_scoped_receiver(scope.owner);
                    evaluated
                })()),
                _ => None,
            }
        }

        fn evaluate_expression<S: SignatureSemantics>(
            semantics: &S,
            expression: SigExprId,
            graph: &SignatureGraph,
            demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
            memo: &mut HashMap<SigExprId, ResolvedTy>,
            computing: &mut std::collections::HashSet<SigExprId>,
        ) -> Result<ResolvedTy, DiagnosticId> {
            if let Some(ty) = memo.get(&expression).copied() {
                return Ok(ty);
            }
            assert!(
                computing.insert(expression),
                "a compact signature expression graph must be acyclic; declaration recursion uses DeclarationType"
            );
            let node = graph
                .expr(expression)
                .expect("a signature expression id must belong to its graph");
            let ty = match node {
                SigExpr::Known(ty) => Ok(ty),
                SigExpr::DeclarationType(declaration) => {
                    demand(declaration).map(|signature| signature.result)
                }
                SigExpr::ClassifierType { declaration, scope } => {
                    let scope = graph
                        .scope(scope)
                        .expect("a classifier type must retain its declaration scope");
                    semantics.classifier_type(declaration, scope)
                }
                SigExpr::Parameter { declaration, index } => semantics
                    .declaration_parameters(declaration)?
                    .get(index as usize)
                    .copied()
                    .ok_or_else(|| semantics.missing_signature_diagnostic(declaration)),
                SigExpr::Type {
                    syntax,
                    scope,
                    origin,
                } => {
                    let scope = graph
                        .scope(scope)
                        .expect("a compact type expression must retain its declaration scope");
                    semantics.resolve_type(scope, origin, syntax, graph)
                }
                SigExpr::ContextualType {
                    expected,
                    syntax,
                    scope,
                    origin,
                } => {
                    let expected =
                        evaluate_expression(semantics, expected, graph, demand, memo, computing)?;
                    let scope = graph
                        .scope(scope)
                        .expect("a contextual type expression must retain its declaration scope");
                    semantics.resolve_contextual_type(scope, origin, syntax, expected, graph)
                }
                SigExpr::Value(selection) => {
                    let selection = graph
                        .value_selection(selection)
                        .expect("a deferred value selection must belong to its graph");
                    let scope = graph
                        .scope(selection.scope)
                        .expect("a deferred value scope must belong to its graph");
                    let spelling = graph
                        .name(selection.spelling)
                        .expect("a deferred value spelling must belong to its graph");
                    let expected = match selection.expected {
                        Some(expected) => Some(evaluate_expression(
                            semantics, expected, graph, demand, memo, computing,
                        )?),
                        None => None,
                    };
                    semantics.select_value(scope, spelling, selection.origin, expected, demand)
                }
                SigExpr::Call { target, arguments } => evaluate_call(
                    semantics, target, arguments, None, graph, demand, memo, computing,
                ),
                SigExpr::CallableReference(target) => {
                    let selection = graph
                        .callable_selection(target)
                        .expect("a deferred callable-reference selection must belong to its graph");
                    let expected = selection
                        .expected
                        .map(|expected| {
                            evaluate_expression(semantics, expected, graph, demand, memo, computing)
                        })
                        .transpose()?;
                    evaluate_callable_reference(
                        semantics, expression, expected, graph, demand, memo, computing,
                    )
                }
                SigExpr::BoundCallableReference { target, .. } => {
                    let selection = graph.callable_selection(target).expect(
                        "a deferred bound callable-reference selection must belong to its graph",
                    );
                    let expected = selection
                        .expected
                        .map(|expected| {
                            evaluate_expression(semantics, expected, graph, demand, memo, computing)
                        })
                        .transpose()?;
                    evaluate_callable_reference(
                        semantics, expression, expected, graph, demand, memo, computing,
                    )
                }
                SigExpr::ClassLiteral {
                    receiver,
                    classifier,
                    scope,
                    root,
                } => {
                    let use_value = match (classifier, root) {
                        (Some(_), Some(root)) => {
                            let scope = graph
                                .scope(scope)
                                .expect("a class literal scope must belong to its graph");
                            let root = graph
                                .name(root)
                                .expect("a class literal root must belong to its graph");
                            semantics.class_literal_receiver_is_value(scope, root)?
                        }
                        (None, None) => true,
                        (Some(_), None) | (None, Some(_)) => {
                            unreachable!("class literal classifier and root must travel together")
                        }
                    };
                    let selected = if use_value {
                        receiver
                    } else {
                        classifier.expect("a classifier candidate must exist")
                    };
                    let selected =
                        evaluate_expression(semantics, selected, graph, demand, memo, computing)?;
                    semantics.class_literal_type(selected, !use_value)
                }
                SigExpr::Member {
                    receiver,
                    lookup,
                    origin,
                } => {
                    let selection = graph
                        .member_selection(lookup)
                        .expect("a deferred member selection must belong to its graph");
                    debug_assert_eq!(selection.origin, origin);
                    let scope = graph
                        .scope(selection.scope)
                        .expect("a deferred member scope must belong to its graph");
                    let spelling = graph
                        .name(selection.spelling)
                        .expect("a deferred member spelling must belong to its graph");
                    // `::property.isInitialized` is a compiler-defined operation on the selected
                    // `lateinit` DECLARATION, not an ordinary member of the reflective property's
                    // runtime type. Preserve that semantic selection in Pass 1 instead of reducing
                    // the reference to `KProperty*` and asking library lookup to rediscover it.
                    if spelling == "isInitialized" {
                        match graph.expr(receiver) {
                            Some(SigExpr::CallableReference(target)) => {
                                let reference = graph.callable_selection(target).expect(
                                    "a deferred callable-reference selection must belong to its graph",
                                );
                                let reference_scope = graph.scope(reference.scope).expect(
                                    "a deferred callable-reference scope must belong to its graph",
                                );
                                let reference_spelling = graph.name(reference.spelling).expect(
                                    "a deferred callable-reference spelling must belong to its graph",
                                );
                                return semantics.select_lateinit_initialized(
                                    reference_scope,
                                    reference_spelling,
                                    reference.origin,
                                    None,
                                    false,
                                    demand,
                                );
                            }
                            Some(SigExpr::BoundCallableReference {
                                receiver,
                                classifier,
                                scope: reference_scope,
                                root,
                                target,
                            }) => {
                                let reference = graph.callable_selection(target).expect(
                                    "a deferred callable-reference selection must belong to its graph",
                                );
                                let reference_spelling = graph.name(reference.spelling).expect(
                                    "a deferred callable-reference spelling must belong to its graph",
                                );
                                let use_value = match (classifier, root) {
                                    (Some(_), Some(root)) => {
                                        let reference_scope = graph.scope(reference_scope).expect(
                                            "a qualified callable-reference scope must belong to its graph",
                                        );
                                        let root = graph.name(root).expect(
                                            "a qualified callable-reference root must belong to its graph",
                                        );
                                        semantics.callable_reference_receiver_is_value(
                                            reference_scope,
                                            root,
                                            reference_spelling,
                                        )?
                                    }
                                    (None, None) => true,
                                    (Some(_), None) | (None, Some(_)) => unreachable!(
                                        "callable-reference classifier and root must travel together"
                                    ),
                                };
                                let selected = if use_value {
                                    receiver
                                } else {
                                    classifier.expect("a classifier candidate must exist")
                                };
                                let selected = evaluate_expression(
                                    semantics, selected, graph, demand, memo, computing,
                                )?;
                                let reference_scope = graph.scope(reference.scope).expect(
                                    "a deferred callable-reference scope must belong to its graph",
                                );
                                return semantics.select_lateinit_initialized(
                                    reference_scope,
                                    reference_spelling,
                                    reference.origin,
                                    Some(selected),
                                    !use_value,
                                    demand,
                                );
                            }
                            _ => {}
                        }
                    }
                    let receiver =
                        evaluate_expression(semantics, receiver, graph, demand, memo, computing)?;
                    let expected = match selection.expected {
                        Some(expected) => Some(evaluate_expression(
                            semantics, expected, graph, demand, memo, computing,
                        )?),
                        None => None,
                    };
                    semantics.select_member(scope, spelling, origin, receiver, expected, demand)
                }
                SigExpr::MemberCall {
                    receiver,
                    target,
                    arguments,
                    origin,
                } => {
                    let receiver =
                        evaluate_expression(semantics, receiver, graph, demand, memo, computing)?;
                    let selection = graph
                        .member_selection(target)
                        .expect("a deferred member-call selection must belong to its graph");
                    debug_assert_eq!(selection.origin, origin);
                    let scope = graph
                        .scope(selection.scope)
                        .expect("a deferred member-call scope must belong to its graph");
                    let spelling = graph
                        .name(selection.spelling)
                        .expect("a deferred member-call spelling must belong to its graph");
                    let mut resolved_type_arguments = Vec::new();
                    for argument in graph.operands(selection.type_arguments).iter().copied() {
                        resolved_type_arguments.push(evaluate_expression(
                            semantics, argument, graph, demand, memo, computing,
                        )?);
                    }
                    let expected = match selection.expected {
                        Some(expected) => Some(evaluate_expression(
                            semantics, expected, graph, demand, memo, computing,
                        )?),
                        None => None,
                    };
                    let probes = graph
                        .call_arguments(arguments)
                        .iter()
                        .map(|argument| {
                            argument_probe(semantics, argument, graph, demand, memo, computing)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let expectations = semantics.member_call_argument_expectations(
                        scope,
                        spelling,
                        origin,
                        receiver,
                        &probes,
                        &resolved_type_arguments,
                        selection.trailing_lambda,
                        expected,
                        demand,
                    )?;
                    let mut resolved_arguments = Vec::new();
                    for (index, argument) in graph.call_arguments(arguments).iter().enumerate() {
                        resolved_arguments.push(materialize_argument(
                            semantics,
                            argument,
                            expectations.get(index).copied().flatten(),
                            graph,
                            demand,
                            memo,
                            computing,
                        )?);
                    }
                    let selected = semantics.select_member_call(
                        scope,
                        spelling,
                        origin,
                        receiver,
                        &resolved_arguments,
                        &resolved_type_arguments,
                        selection.trailing_lambda,
                        expected,
                        demand,
                    )?;
                    if let Some(effect) = selected
                        .declaration
                        .and_then(|declaration| graph.local_effect(declaration))
                    {
                        semantics.enter_scoped_receiver(scope.owner, receiver);
                        let evaluated = evaluate_expression(
                            semantics,
                            effect.result,
                            graph,
                            demand,
                            memo,
                            computing,
                        );
                        semantics.exit_scoped_receiver(scope.owner);
                        let effect_result = evaluated?;
                        Ok(if effect.determines_result {
                            effect_result
                        } else {
                            selected.ty.ok_or_else(|| {
                                semantics.missing_signature_diagnostic(scope.owner)
                            })?
                        })
                    } else {
                        selected
                            .ty
                            .ok_or_else(|| semantics.missing_signature_diagnostic(scope.owner))
                    }
                }
                SigExpr::Binary {
                    operator,
                    lhs,
                    rhs,
                    scope,
                    origin,
                } => {
                    // Evaluate both independent operands even when the first one fails. Production
                    // semantics records source diagnostics while evaluating a dependency (for
                    // example, both eager reads in `val x = later + after`); returning after the
                    // left failure would make diagnostic completeness depend on operand order.
                    let lhs = evaluate_expression(semantics, lhs, graph, demand, memo, computing);
                    let rhs = evaluate_expression(semantics, rhs, graph, demand, memo, computing);
                    let (lhs, rhs) = match (lhs, rhs) {
                        (Ok(lhs), Ok(rhs)) => (lhs, rhs),
                        (Err(diagnostic), _) | (Ok(_), Err(diagnostic)) => {
                            return Err(diagnostic);
                        }
                    };
                    let scope = graph
                        .scope(scope)
                        .expect("a binary signature expression must retain its declaration scope");
                    semantics.select_binary(scope, operator, origin, lhs, rhs, demand)
                }
                SigExpr::Invoke {
                    callee,
                    arguments,
                    scope,
                    origin,
                } => {
                    let callee =
                        evaluate_expression(semantics, callee, graph, demand, memo, computing)?;
                    let probes = graph
                        .call_arguments(arguments)
                        .iter()
                        .map(|argument| {
                            argument_probe(semantics, argument, graph, demand, memo, computing)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let scope = graph
                        .scope(scope)
                        .expect("an invoke signature expression must retain its declaration scope");
                    let expectations =
                        semantics.invoke_argument_expectations(scope, callee, &probes)?;
                    let mut resolved_arguments = Vec::new();
                    for (index, argument) in graph.call_arguments(arguments).iter().enumerate() {
                        resolved_arguments.push(materialize_argument(
                            semantics,
                            argument,
                            expectations.get(index).copied().flatten(),
                            graph,
                            demand,
                            memo,
                            computing,
                        )?);
                    }
                    semantics.select_invoke(scope, origin, callee, &resolved_arguments, demand)
                }
                SigExpr::Function {
                    parameters,
                    result,
                    context_count,
                    has_receiver,
                    suspend,
                } => {
                    let mut resolved_parameters = Vec::new();
                    for parameter in graph.operands(parameters).iter().copied() {
                        resolved_parameters.push(evaluate_expression(
                            semantics, parameter, graph, demand, memo, computing,
                        )?);
                    }
                    let result =
                        evaluate_expression(semantics, result, graph, demand, memo, computing)?;
                    semantics.make_function_type(
                        &resolved_parameters,
                        result,
                        context_count,
                        has_receiver,
                        suspend,
                    )
                }
                SigExpr::ContextualParameter(declaration) => {
                    Err(semantics.missing_signature_diagnostic(declaration))
                }
                SigExpr::ContextualFunction { parameters, .. } => {
                    let declaration = graph
                        .operands(parameters)
                        .iter()
                        .find_map(|parameter| match graph.expr(*parameter) {
                            Some(SigExpr::ContextualParameter(declaration)) => Some(declaration),
                            _ => None,
                        })
                        .unwrap_or_else(|| DeclarationId::from_raw(0));
                    Err(semantics.missing_signature_diagnostic(declaration))
                }
                SigExpr::ScopedReceiver {
                    receiver,
                    result,
                    scope,
                } => {
                    let receiver =
                        evaluate_expression(semantics, receiver, graph, demand, memo, computing)?;
                    let scope = graph
                        .scope(scope)
                        .expect("a scoped receiver must retain its declaration scope");
                    semantics.enter_scoped_receiver(scope.owner, receiver);
                    let evaluated =
                        evaluate_expression(semantics, result, graph, demand, memo, computing);
                    semantics.exit_scoped_receiver(scope.owner);
                    evaluated
                }
                SigExpr::Sequence { effects, result } => {
                    crate::trace_compiler!(
                        "signature",
                        "evaluate sequence effects={:?} result={result:?} result_node={:?}",
                        graph.operands(effects),
                        graph.expr(result),
                    );
                    for effect in graph.operands(effects).iter().copied() {
                        evaluate_expression(semantics, effect, graph, demand, memo, computing)?;
                    }
                    evaluate_expression(semantics, result, graph, demand, memo, computing)
                }
                SigExpr::Delegate {
                    declaration,
                    delegate,
                    scope,
                    origin,
                    local,
                } => {
                    let delegate =
                        evaluate_expression(semantics, delegate, graph, demand, memo, computing)?;
                    let scope = graph
                        .scope(scope)
                        .expect("a delegated signature must retain its declaration scope");
                    semantics.select_delegate(declaration, scope, origin, delegate, local, demand)
                }
                SigExpr::Join {
                    operands,
                    scope,
                    origin,
                } => {
                    let operand_expressions = graph.operands(operands).to_vec();
                    let mut resolved_operands = Vec::new();
                    for operand in operand_expressions.iter().copied() {
                        resolved_operands.push(evaluate_expression(
                            semantics, operand, graph, demand, memo, computing,
                        )?);
                    }
                    let scope = graph
                        .scope(scope)
                        .expect("a signature join scope must belong to its graph");
                    // Conditional branches constrain return-only generic calls in either source
                    // order. First evaluate every branch freely, then re-evaluate only a branch
                    // whose result is the semantic supertype of its siblings. Supplying that
                    // narrower sibling as the ordinary call-result expectation lets the resolver's
                    // generic inference specialize `materialize<T>()`; concrete or incompatible
                    // calls remain unchanged. Iteration computes the small branch-local fixed point
                    // without adding a second inference algorithm to the graph evaluator.
                    for _ in 0..resolved_operands.len() {
                        let mut changed = false;
                        for (index, &operand) in operand_expressions.iter().enumerate() {
                            let siblings = resolved_operands
                                .iter()
                                .enumerate()
                                .filter(|(other, _)| *other != index)
                                .map(|(_, sibling)| *sibling)
                                .collect::<Vec<_>>();
                            let Some(first_sibling) = siblings.first().copied() else {
                                continue;
                            };
                            let sibling = if siblings.len() == 1 {
                                first_sibling
                            } else {
                                semantics.least_upper_bound(scope, origin, &siblings)?
                            };
                            let current = resolved_operands[index];
                            let joined =
                                semantics.least_upper_bound(scope, origin, &[current, sibling])?;
                            crate::trace_compiler!(
                                "signature",
                                "conditional constraint operand={operand:?} current={:?} sibling={:?} joined={:?}",
                                current.get(),
                                sibling.get(),
                                joined.get(),
                            );
                            if current == sibling
                                || (!current.get().mentions_ty_param() && joined != current)
                            {
                                continue;
                            }
                            let Some(Ok(rebound)) = evaluate_expression_with_expected(
                                semantics, operand, sibling, graph, demand, memo, computing,
                            ) else {
                                continue;
                            };
                            if rebound != current {
                                resolved_operands[index] = rebound;
                                changed = true;
                            }
                        }
                        if !changed {
                            break;
                        }
                    }
                    semantics.least_upper_bound(scope, origin, &resolved_operands)
                }
                SigExpr::Nullable(base) => semantics.make_nullable(evaluate_expression(
                    semantics, base, graph, demand, memo, computing,
                )?),
                SigExpr::NonNullable(base) => semantics.make_non_nullable(evaluate_expression(
                    semantics, base, graph, demand, memo, computing,
                )?),
                SigExpr::Substitute {
                    base,
                    substitutions,
                } => {
                    let base =
                        evaluate_expression(semantics, base, graph, demand, memo, computing)?;
                    let mut resolved_substitutions = Vec::new();
                    for substitution in graph.substitutions(substitutions) {
                        let value = evaluate_expression(
                            semantics,
                            substitution.value,
                            graph,
                            demand,
                            memo,
                            computing,
                        )?;
                        resolved_substitutions.push((substitution.parameter, value));
                    }
                    semantics.substitute(base, &resolved_substitutions)
                }
            };
            computing.remove(&expression);
            if ty.is_err() {
                crate::trace_compiler!(
                    "signature",
                    "signature expression {expression:?} declined: {:?}",
                    graph.expr(expression),
                );
            }
            let ty = ty?;
            memo.insert(expression, ty);
            Ok(ty)
        }

        let result = evaluate_expression(
            self.semantics,
            result,
            graph,
            demand,
            &mut HashMap::new(),
            &mut std::collections::HashSet::new(),
        )?;
        let result = self
            .semantics
            .approximate_declaration_result(declaration, result)?;
        let parameters = self.semantics.declaration_parameters(declaration);
        if parameters.is_err() {
            crate::trace_compiler!(
                "signature",
                "declaration_parameters unavailable for {declaration:?}",
            );
        }
        Ok(ResolvedSignature {
            parameters: parameters?,
            result,
        })
    }

    fn recursive_inference_diagnostic(&self, declaration: DeclarationId) -> DiagnosticId {
        self.semantics.recursive_inference_diagnostic(declaration)
    }

    fn missing_signature_diagnostic(&self, declaration: DeclarationId) -> DiagnosticId {
        self.semantics.missing_signature_diagnostic(declaration)
    }
}
