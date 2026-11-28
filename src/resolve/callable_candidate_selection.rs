//! Applicability and ranking of one already-collected callable candidate family.

use super::*;

/// Whether an argument of exactly the parameter's type takes the top specificity rank. A type
/// parameter is not such evidence: whether `X` fits better than its bound is decided by comparing
/// the declarations, so two overloads over `X` and `Right` stay tied.
pub(super) fn is_exact_argument_rank(expected: Ty, actual: Ty) -> bool {
    expected == actual && !matches!(expected.kind(), TyKind::TyParam(..) | TyKind::Var(..))
}

/// The call-site signature's parameters, before inference binds its variables, in the order of
/// the scored shape. `None` when the shape names a parameter the signature does not declare.
fn shaped_declared_parameters(
    signature: &crate::libraries::GenericSig,
    shape: &ContextualCallShape,
) -> Option<Vec<Ty>> {
    shape
        .parameter_indices
        .iter()
        .map(|&index| signature.params.get(index).copied())
        .collect()
}

impl Checker<'_> {
    /// Select one declaration from a provider's raw callable family. Receiver/name lookup has already
    /// produced the family; applicability, generic binding, and contextual argument shape are shared.
    pub(super) fn select_callable_candidate(
        &self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        type_args: &[Ty],
        extension_receiver: Option<Ty>,
        result_constraint: CallResultConstraint,
        overloads: Vec<crate::libraries::FunctionInfo>,
    ) -> Option<CallableCandidateSelection> {
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let argument_names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        // Context-sensitive syntax has no standalone argument type. Callers carry `Ty::Error` in
        // the dense argument vector until a candidate supplies its function/SAM shape; expose that
        // state to the shared scorer as an absent partial type so it uses `lambda_slot` instead of
        // treating the error sentinel as an ordinary value assignable to every parameter. A real
        // erroneous non-functional expression remains `Some(Ty::Error)` and follows normal error
        // propagation.
        let partial = args
            .iter()
            .zip(arg_tys)
            .map(|(&argument, &ty)| {
                (!(ty == Ty::Error
                    && matches!(
                        self.file.expr(argument),
                        Expr::Lambda { .. } | Expr::CallableRef { .. }
                    )))
                .then_some(ty)
            })
            .collect::<Vec<_>>();
        let shape_without_implicit_context =
            |params: &[Ty], call_sig: &CallSig, context_count: usize| {
                if context_count == 0 || context_count > params.len() {
                    return None;
                }
                let explicitly_named = |parameter: usize| {
                    self.file.explicit_context_arguments
                        && call_sig
                            .param_names
                            .get(parameter)
                            .is_some_and(|parameter| {
                                argument_names.is_some_and(|names| {
                                    names.iter().flatten().any(|argument| argument == parameter)
                                })
                            })
                };
                let parameter_indices = (context_count..params.len())
                    .chain((0..context_count).filter(|&parameter| explicitly_named(parameter)))
                    .collect::<Vec<_>>();
                Some(ContextualCallShape {
                    params: parameter_indices
                        .iter()
                        .map(|&parameter| params[parameter])
                        .collect(),
                    call_sig: call_sig.select_parameters(&parameter_indices),
                    parameter_indices,
                    context_sources: vec![None; context_count],
                    context_actual_types: vec![None; context_count],
                })
            };
        let lexical_formals = scope.lexical_tparam_identities();
        let mut applicable = Vec::new();
        let mut sam_converted = crate::fx_hash::HashSet::default();
        let mut misfits = crate::fx_hash::HashSet::default();
        for (index, mut candidate) in overloads.iter().cloned().enumerate() {
            // Inference inputs belong to this call, not to the declaration selection publishes.
            let constructor_constraints = candidate.take_constructor_call_constraints();
            let type_args =
                constructor_constraints.map_or(type_args, |constraints| constraints.type_arguments);
            let result_constraint = constructor_constraints
                .map_or(result_constraint, |constraints| {
                    CallResultConstraint::direct(constraints.expected)
                });
            let expected = result_constraint.expected();
            if self.file.infix_calls.contains(&call.0) && !candidate.flags.infix {
                continue;
            }
            if !self.source_callable_visible(&candidate) {
                crate::trace_compiler!(
                    "resolve",
                    "candidate {} rejected: not visible",
                    candidate.callable.name,
                );
                continue;
            }
            // Selection solves the call's own variables; what it publishes is declared again.
            let declared_signature = candidate.semantic_signature();
            let call_variables = crate::symbol_resolver::CallSiteVariables::instantiate(
                &declared_signature,
                self.call_site(call),
            );
            let signature = call_variables.signature().clone();
            let shaped = call_site_publication::ShapedArguments::new(
                &declared_signature,
                self.call_site(call),
                &lexical_formals,
            );
            let (arg_tys, partial) = (&shaped.types(arg_tys)[..], shaped.partial(&partial));
            let result_signature = result_constraint.signature(&signature);
            let candidate_receiver = extension_receiver.map(|receiver| {
                if candidate.kind == crate::libraries::FnKind::Member {
                    crate::symbol_resolver::member_scope_receiver(receiver)
                } else {
                    receiver
                }
            });
            if !type_args.is_empty() && signature.formals.len() != type_args.len() {
                crate::trace_compiler!(
                    "resolve",
                    "candidate {} rejected: {} explicit type arguments for {} formals",
                    candidate.callable.name,
                    type_args.len(),
                    signature.formals.len(),
                );
                continue;
            }
            let (initial_shape, missing_context) = match self.contextual_call_shape(
                scope,
                &signature.params,
                &candidate.call_sig,
                candidate.context_count,
                argument_names,
            ) {
                Some(shape) => (shape, false),
                None => (
                    shape_without_implicit_context(
                        &signature.params,
                        &candidate.call_sig,
                        candidate.context_count,
                    )?,
                    true,
                ),
            };
            let Some(parameters) = call_argument_parameter_indices(
                args.len(),
                initial_shape.params.len(),
                argument_names,
                trailing_lambda,
                &initial_shape.call_sig,
            ) else {
                crate::trace_compiler!(
                    "resolve",
                    "candidate {} rejected: arguments do not map to params={:?} call_sig={:?}",
                    candidate.callable.name,
                    initial_shape.params,
                    initial_shape.call_sig,
                );
                continue;
            };
            let whole_arrays =
                named_whole_array_varargs(&parameters, argument_names, &initial_shape.call_sig);
            let mut bindings = crate::symbol_resolver::seeded_gsig_binds(&signature, type_args);
            let source = self.fed_source();
            let argument_kinds = shaped.kinds(self.checked_call_arg_kinds(scope, args));
            let receiver_expression = match self.file.expr(call) {
                Expr::Call { callee, .. } => match self.file.expr(*callee) {
                    Expr::Member { receiver, .. } => Some(*receiver),
                    _ => None,
                },
                _ => None,
            };
            let mut inference_receiver = None;
            if let (Some(actual), Some(declared)) = (candidate_receiver, signature.receiver) {
                // Receiver applicability may project a nominal callable classifier through its
                // exact function-type supertype. Generic binding must consume that same semantic
                // receiver (`Task : suspend () -> Int` binds the `T` in `suspend () -> T`), while
                // the nominal expression remains the runtime receiver recorded below.
                let actual = if declared.non_null().is_fun() {
                    crate::symbol_resolver::classifier_callable_signature(&source, actual)
                        .unwrap_or(actual)
                } else {
                    actual
                };
                inference_receiver = Some(actual);
                crate::symbol_resolver::unify_ty_from_symbols(
                    &source,
                    declared,
                    actual,
                    &mut bindings,
                );
            }
            let mut receiver_bindings = bindings.clone();
            for (parameter, actual) in initial_shape
                .context_actual_types
                .iter()
                .enumerate()
                .filter_map(|(parameter, actual)| actual.map(|actual| (parameter, actual)))
            {
                if !candidate
                    .call_sig
                    .parameter_contributes_to_inference(parameter)
                {
                    continue;
                }
                crate::symbol_resolver::unify_ty(
                    signature.params[parameter],
                    actual,
                    &mut bindings,
                );
            }
            // Receiver, explicit-type-argument, and context-argument evidence is fixed before
            // ordinary value arguments participate. Preserve that boundary so an invariant
            // expected result can replace a provisional value-argument solution without replacing
            // a receiver or context equality.
            let non_value_argument_bindings = bindings.clone();
            // A stable path can have a source type that is an intersection even though ordinary
            // expression storage exposes only the last smart-cast projection. When such a value
            // directly constrains a callable formal, infer a short-lived type-parameter witness:
            // its first bound is the runtime/erasure view and this candidate-local table carries the
            // remaining proven constituents. The witness never leaves selection; checked FIR gets
            // the primary type plus explicit additional bounds below.
            let mut flow_intersections = HashMap::<&'static str, Vec<Ty>>::default();
            let inference_actuals = parameters
                .iter()
                .copied()
                .zip(arg_tys.iter().copied())
                .zip(&argument_kinds)
                .zip(&whole_arrays)
                .enumerate()
                .flat_map(
                    |(argument, (((parameter, _raw_actual), kind), named_whole_array))| {
                        if self
                            .unbound_contextual_result_signature(args[argument])
                            .is_some()
                        {
                            return None;
                        }
                        let whole_array =
                            *named_whole_array || self.file.is_spread_arg(args[argument]);
                        let declaration_parameter = initial_shape.parameter_indices[parameter];
                        if !candidate
                            .call_sig
                            .parameter_contributes_to_inference(declaration_parameter)
                        {
                            return None;
                        }
                        let mut declared = signature.params[declaration_parameter];
                        if candidate.call_sig.vararg_index == Some(declaration_parameter)
                            && !whole_array
                        {
                            declared = declared.array_read_elem().unwrap_or(declared);
                        }
                        let mut actual = kind.inference_type(&source, declared);
                        // `@Exact` compares the argument's complete semantic type. Replacing an
                        // already-recorded intersection with a fresh per-argument witness makes two
                        // occurrences of the same value look like different types and rejects
                        // `checkTypeEquality(value, value)`. Flow witnesses are only needed for an
                        // ordinary parameter that may select one constituent during applicability.
                        if !whole_array
                            && !candidate
                                .call_sig
                                .exact_params
                                .get(declaration_parameter)
                                .copied()
                                .unwrap_or(false)
                        {
                            if let TyKind::TyParam(formal, _) | TyKind::Var(formal, _) =
                                declared.non_null().kind()
                            {
                                let intersection = self
                                    .expr_access_path(args[argument])
                                    .map(|path| self.lookup_intersection_narrowing(scope, &path))
                                    .unwrap_or_default();
                                if intersection.len() > 1 {
                                    let identity = crate::types::intern(&format!(
                                        "\0flow-intersection:{}:{}:{}",
                                        call.0, argument, formal
                                    ));
                                    let proven = Ty::intersection(&intersection);
                                    flow_intersections.insert(identity, intersection);
                                    actual = Ty::ty_param(identity, proven);
                                }
                            }
                        }
                        // Each conditional branch passes through the same argument view as a
                        // whole argument: integer-literal adaptation, SAM/function shape.
                        let branches = match conditional_branch::conditional_argument_branches(
                            self.file,
                            args[argument],
                        ) {
                            Some(branches) if !whole_array => branches
                                .into_iter()
                                .map(|branch| {
                                    self.call_arg_kind(scope, branch)
                                        .inference_type(&source, declared)
                                })
                                .collect(),
                            _ => vec![actual],
                        };
                        Some(branches.into_iter().map(move |actual| {
                            (argument, declaration_parameter, actual, whole_array)
                        }))
                    },
                )
                .flatten()
                .collect::<Vec<_>>();
            // Kotlin integer literals are adaptable constraints, not fixed primitive types. Let
            // non-literal arguments solve first; when they already fix every formal touched by a
            // literal to a representable type, the literal adds no competing lower bound. Thus the
            // `0` in `foldl(0) { _, acc: Long -> ... }` conforms to `A = Long` instead of widening
            // the lambda's independent `A = Long` constraint with a premature `A = Int` to `Any`.
            // With no other evidence the literal remains in the ordinary solver and still infers
            // `Int` for calls such as `identity(0)`.
            let preliminary =
                crate::symbol_resolver::infer_generic_call_constraints_with_receiver_from_symbols(
                    &source,
                    &signature,
                    inference_receiver,
                    inference_actuals.iter().filter_map(
                        |&(argument, parameter, actual, whole_array)| {
                            (!matches!(
                                argument_kinds[argument],
                                CallArgKind::IntegerLiteral { .. }
                                    | CallArgKind::CollectionLiteral { .. }
                            ))
                            .then_some((
                                parameter,
                                actual,
                                whole_array,
                            ))
                        },
                    ),
                    candidate.call_sig.vararg_index,
                );
            let expected_literal_targets = expected.and_then(|expected| {
                crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
                    &source,
                    &signature,
                    expected,
                    |actual, bound| self.receiver_is_assignable(actual, bound),
                )
            });
            let adapted_inference_actuals = inference_actuals
                .iter()
                .filter_map(|&(argument, parameter, actual, whole_array)| {
                    let mut actual = (parameter, actual, whole_array);
                    let kind = match argument_kinds[argument].clone() {
                        CallArgKind::IntegerLiteral { ty, constant } => {
                            CallArgKind::integer_constant(
                                self.integer_literal_semantic_type(ty),
                                constant,
                            )
                        }
                        kind => kind,
                    };
                    if matches!(kind, CallArgKind::CollectionLiteral { .. }) {
                        // A collection literal is built from the type its parameter ends up with,
                        // so its expectation-free `List<T>` reading is evidence only for a formal
                        // nothing else constrains: `select(mutableSetOf<Int>(), [1])` is a
                        // `MutableSet<Int>`, not the `Collection<Int>` joined with that reading.
                        let literal =
                            crate::symbol_resolver::infer_generic_call_constraints_from_symbols(
                                &source,
                                &signature,
                                [actual],
                                candidate.call_sig.vararg_index,
                            );
                        let constrained_elsewhere = !literal.bindings.is_empty()
                            && literal
                                .bindings
                                .keys()
                                .all(|formal| preliminary.bindings.contains_key(formal));
                        return (!constrained_elsewhere).then_some(actual);
                    }
                    if !matches!(kind, CallArgKind::IntegerLiteral { .. }) {
                        return Some(actual);
                    }
                    let literal =
                        crate::symbol_resolver::infer_generic_call_constraints_from_symbols(
                            &source,
                            &signature,
                            [actual],
                            candidate.call_sig.vararg_index,
                        );
                    let adapts_to = |target: Ty| {
                        kind.adapts_integer_literal_to(self.integer_literal_semantic_type(target))
                    };
                    let adapts_to_existing = !literal.bindings.is_empty()
                        && literal.bindings.keys().all(|formal| {
                            preliminary
                                .bindings
                                .get(formal)
                                .is_some_and(|&target| adapts_to(target))
                        });
                    if adapts_to_existing {
                        return None;
                    }
                    // The literal is an adaptable lower bound, not a fixed `Int`/`UInt`: when the
                    // result expectation bounds its formal by a type the literal adapts to
                    // (`List<UShort?>`, `Array<Short?>`), it constrains the formal as that type,
                    // joining the other evidence (`null`) under the same expected upper bound.
                    if let Some(target) = literal.bindings.keys().find_map(|formal| {
                        expected_literal_targets
                            .as_ref()
                            .and_then(|targets| targets.get(formal))
                            .copied()
                            .filter(|&target| adapts_to(target.non_null()))
                    }) {
                        if literal.bindings.len() == 1 {
                            actual.1 = self.integer_literal_semantic_type(target.non_null());
                            return Some(actual);
                        }
                    }
                    // When a literal is the only evidence for a directly declared type
                    // parameter, its scalar upper bound is its contextual type. This keeps
                    // `f(1u)` at `T = UByte` for `<T : UByte>` while an unbounded
                    // `identity(1u)` still takes the ordinary `T = UInt` path. Library
                    // signatures may expose the bound as its reference classifier, so ask the
                    // semantic platform for the equivalent value form before testing the
                    // literal range and feeding the constraint solver.
                    let mut declared = signature.params[actual.0];
                    if candidate.call_sig.vararg_index == Some(actual.0) && !actual.2 {
                        declared = declared.array_read_elem().unwrap_or(declared);
                    }
                    let mut bounded = declared.non_null();
                    while let TyKind::TyParam(_, bound) | TyKind::Var(_, bound) = bounded.kind() {
                        let value_bound = self.integer_literal_semantic_type(bound);
                        if kind.adapts_integer_literal_to(value_bound) {
                            actual.1 = value_bound;
                            break;
                        }
                        bounded = bound.non_null();
                    }
                    Some(actual)
                })
                .collect::<Vec<_>>();
            let mut inferred =
                crate::symbol_resolver::infer_generic_call_constraints_with_receiver_from_symbols(
                    &source,
                    &signature,
                    inference_receiver,
                    adapted_inference_actuals.iter().copied(),
                    candidate.call_sig.vararg_index,
                );
            // A lambda literal used as an extension receiver is still context-sensitive. Only a
            // value-argument constraint that proves the receiver is Unit-coercible may replace its
            // provisional result. Rerun the shared solver with that proven semantic receiver so
            // ordinary receiver/dependent-bound evidence is retained on every other path.
            let mut unit_coerced_receiver = None;
            if let (Some(actual_receiver), Some(declared_receiver)) =
                (candidate_receiver, signature.receiver)
            {
                if let Some(receiver_expression) = receiver_expression {
                    'receiver_constraint: for (formal, upper_bounds) in &inferred.upper_bounds {
                        let Some(formal_index) = signature
                            .formals
                            .iter()
                            .position(|declared| declared == formal)
                        else {
                            continue;
                        };
                        if type_args
                            .get(formal_index)
                            .is_some_and(|argument| *argument != Ty::Error)
                        {
                            continue;
                        }
                        for &upper in upper_bounds {
                            let mut trial = bindings.clone();
                            trial.insert(crate::types::TyParamName::new(formal), upper);
                            let expected_receiver = crate::symbol_resolver::ty_subst_keep_unbound(
                                declared_receiver,
                                &trial,
                            );
                            if let Some(coerced) = self.unit_coerced_lambda_type(
                                receiver_expression,
                                expected_receiver,
                                actual_receiver,
                            ) {
                                unit_coerced_receiver = Some((formal.clone(), upper, coerced));
                                break 'receiver_constraint;
                            }
                        }
                    }
                }
            }
            if let Some((formal, upper, coerced)) = unit_coerced_receiver {
                inference_receiver = Some(coerced);
                bindings.insert(crate::types::TyParamName::new(&formal), upper);
                receiver_bindings.insert(crate::types::TyParamName::new(&formal), upper);
                inferred = crate::symbol_resolver::infer_generic_call_constraints_with_receiver_from_symbols(
                    &source,
                    &signature,
                    Some(coerced),
                    adapted_inference_actuals.iter().copied(),
                    candidate.call_sig.vararg_index,
                );
            }
            let inferred_lower_inputs = inferred.lower_inputs.clone();
            let nullable_flexible_roots = inferred.nullable_flexible_roots.clone();
            let upper_argument_bindings = if candidate.projected_return_hazard {
                crate::symbol_resolver::GSigBinds::default()
            } else {
                inferred.tightest_upper_bindings(&source)
            };
            // Snapshot the denotable upper before `inferred.bindings` moves. Publishing it does
            // not replace a binding the expected result already fixed, so
            // `(): Nothing = intersect(...)` stays `Nothing` while an unconstrained
            // `intersect(In<Int>(), In<String>())` becomes `Int & String`.
            let denotable_upper_bindings = inferred.denotable_upper_bindings(&source);
            crate::symbol_resolver::merge_call_argument_bindings(
                &source,
                &signature,
                type_args,
                &receiver_bindings,
                &mut bindings,
                inferred.bindings,
            );
            crate::symbol_resolver::merge_call_argument_bindings(
                &source,
                &signature,
                type_args,
                &receiver_bindings,
                &mut bindings,
                upper_argument_bindings,
            );
            // Solve an available result expectation before visiting postponed generic arguments.
            // At this point ordinary receiver/value evidence has already won: upper bindings fill
            // only still-open formals. That gives `identity(produce()) : Int` an `Int` parameter
            // expectation for the nested producer instead of prematurely materializing both calls
            // at their `Any?` bounds. Projected-return hazards deliberately retain their parameter-
            // side bottom solution and therefore keep the established later ordering below.
            let expected_result_bindings = expected.and_then(|expected| {
                crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
                    &source,
                    &nested_producer_bindings::solved_bound_signature(&result_signature, &bindings),
                    expected,
                    |actual, bound| self.receiver_is_assignable(actual, bound),
                )
            });
            let expected_return_intersection_bindings = expected_result_bindings.clone();
            if !candidate.projected_return_hazard {
                if let Some(result_bindings) = expected_result_bindings.as_ref() {
                    let unprojected = crate::symbol_resolver::unprojected_expected_result_formals(
                        &source,
                        &result_signature,
                        expected.expect("bindings require an expected result"),
                        result_bindings,
                    );
                    let mut contextual = result_bindings.clone();
                    contextual.retain(|formal, _| unprojected.contains(formal.as_str()));
                    crate::symbol_resolver::merge_generic_upper_bindings(
                        &signature,
                        type_args,
                        &mut bindings,
                        contextual,
                        |actual, upper| self.receiver_is_assignable(actual, upper),
                    );
                }
            }
            // A postponed generic result participates after the other arguments have constrained
            // this candidate. For `mapNotNullTo(mutableSetOf()) { Module? }`, the lambda first fixes
            // `R = Module`; the destination bound then becomes `MutableCollection<in Module>`, which
            // fixes the nested call to `MutableSet<Module>` and finally fixes `C` to that exact type.
            // This is one constraint round over semantic signatures, independent of call/provider
            // spelling; no library classifier or function name is involved.
            for (argument, &visible_parameter) in parameters.iter().enumerate() {
                let Some(generic_sig) = self.unbound_contextual_result_signature(args[argument])
                else {
                    continue;
                };
                let Some(&parameter) = initial_shape.parameter_indices.get(visible_parameter)
                else {
                    continue;
                };
                if !candidate
                    .call_sig
                    .parameter_contributes_to_inference(parameter)
                {
                    continue;
                }
                let Some(&declared_parameter) = signature.params.get(parameter) else {
                    continue;
                };
                // A spread or named array is the whole vararg array; any other argument is one
                // element. The parameter view and the constraint below must agree on that.
                let whole_array = whole_arrays.get(argument).copied().unwrap_or(false)
                    || self.file.is_spread_arg(args[argument]);
                let declared_parameter =
                    if candidate.call_sig.vararg_index == Some(parameter) && !whole_array {
                        declared_parameter
                            .array_read_elem()
                            .unwrap_or(declared_parameter)
                    } else {
                        declared_parameter
                    };
                let open_formal = nested_producer_bindings::open_variable(
                    &signature,
                    type_args,
                    declared_parameter,
                );
                let nested_expected = nested_producer_bindings::producer_expectation(
                    &signature,
                    type_args,
                    &bindings,
                    declared_parameter,
                );
                crate::trace_compiler!(
                    "resolve",
                    "postponed argument call={call:?} parameter={parameter} expected={nested_expected:?} generic_ret={:?}",
                    generic_sig.ret,
                );
                let concrete_nested =
                    match crate::symbol_resolver::nested_result_from_concrete_parameter(
                        &generic_sig,
                        nested_expected,
                        &signature.formals,
                        |actual, bound| self.receiver_is_assignable(actual, bound),
                    ) {
                        Ok(result) => result,
                        Err(()) => continue,
                    };
                let mut nested_bindings = if concrete_nested.is_some() {
                    crate::symbol_resolver::GSigBinds::default()
                } else {
                    match crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
                        &source,
                        &generic_sig,
                        nested_expected,
                        |actual, bound| self.receiver_is_assignable(actual, bound),
                    ) {
                        Some(bindings) => bindings,
                        // The outer input may itself still be an inference variable. It cannot
                        // constrain the nested producer yet, so Kotlin uses the nested call's bottom
                        // instantiation and lets that concrete result constrain the outer variable.
                        // A concrete but incompatible expectation remains a real failure.
                        // With no concrete outer expectation, a nested producer contributes its
                        // proper unconstrained result. That shared operation keeps concrete bounds
                        // and prevents declaration-private variables from escaping this call.
                        None if nested_expected.mentions_ty_param() => {
                            crate::symbol_resolver::unconstrained_result_bindings(&generic_sig)
                        }
                        // An outer variable typed only by another argument's lower bound is not
                        // fixed yet: the nested result becomes a second lower bound, and the
                        // variable their common supertype.
                        None if open_formal.is_some() => {
                            let upper = open_formal.and_then(|formal| {
                                expected_result_bindings.as_ref()?.get(formal).copied()
                            });
                            match nested_producer_bindings::open_variable_bindings(
                                &source,
                                &generic_sig,
                                nested_expected,
                                upper,
                                |actual, bound| self.receiver_is_assignable(actual, bound),
                            ) {
                                Some(bindings) => bindings,
                                None => continue,
                            }
                        }
                        None => continue,
                    }
                };
                nested_producer_bindings::enforce_declared_bounds(
                    &generic_sig,
                    &mut nested_bindings,
                    |actual, bound| self.receiver_is_assignable(actual, bound),
                );
                crate::trace_compiler!(
                    "resolve",
                    "postponed argument call={call:?} parameter={parameter} nested_bindings={nested_bindings:?}",
                );
                let nested_actual = concrete_nested.unwrap_or_else(|| {
                    crate::symbol_resolver::ty_subst_keep_unbound(generic_sig.ret, &nested_bindings)
                });
                let nested_constraints =
                    crate::symbol_resolver::infer_generic_call_constraints_from_symbols(
                        &source,
                        &signature,
                        [(parameter, nested_actual, whole_array)],
                        candidate.call_sig.vararg_index,
                    );
                let mut inferred_nested = nested_constraints.bindings;
                // A formal reached only through an input projection has no lower/equality
                // evidence. The nested producer contributes Kotlin's most-specific bottom
                // solution here; its readable upper is a bound, not a concrete outer binding.
                for formal in nested_constraints.upper_only {
                    inferred_nested
                        .entry(crate::types::TyParamName::new(&formal))
                        .or_insert(Ty::Nothing);
                }
                // The producer's result is a lower bound for a bare outer variable.
                if open_formal.is_some() {
                    nested_producer_bindings::join_lower_bounds(
                        &source,
                        &signature,
                        &receiver_bindings,
                        &mut bindings,
                        &mut inferred_nested,
                    );
                }
                crate::trace_compiler!(
                    "resolve",
                    "postponed argument call={call:?} parameter={parameter} nested_actual={nested_actual:?} outer_bindings={inferred_nested:?}",
                );
                crate::symbol_resolver::merge_call_argument_bindings(
                    &source,
                    &signature,
                    type_args,
                    &receiver_bindings,
                    &mut bindings,
                    inferred_nested,
                );
            }
            // A projected-return input contributes only upper constraints. `Nothing` is the
            // placeholder an expected result can still replace (`select(Context<Any>()): String`).
            // An incompatible expected type must not turn `Context<out T>` passed to
            // `Context<in U>` into `Context<in Expected>`. A concrete upper is published below,
            // after that replacement, and only when the expected result did not fix the variable
            // (`select(Context<Any>())` is `Any`).
            if candidate.projected_return_hazard {
                crate::symbol_resolver::seed_denotable_upper_placeholders(
                    &mut bindings,
                    &denotable_upper_bindings,
                );
            }
            if let (Some(expected), Some(result_bindings)) = (expected, expected_result_bindings) {
                // The expected result also relates through the declared return's supertypes: a Java
                // factory declared `MutableReply<T>` is seen as `Reply<T>` where `Reply<Any>` is
                // expected, and only that applied shape can bind `T`.
                {
                    let widening_bindings = result_bindings
                        .iter()
                        .filter(|(formal, _)| !nullable_flexible_roots.contains(formal.as_str()))
                        .map(|(formal, &binding)| (*formal, binding))
                        .collect::<crate::symbol_resolver::GSigBinds>();
                    crate::symbol_resolver::widen_invariant_expected_bindings(
                        &source,
                        &result_signature,
                        type_args,
                        &mut bindings,
                        &widening_bindings,
                        expected,
                        |actual, bound| self.receiver_is_assignable(actual, bound),
                    );
                    // A nested generic call may already have a concrete probe type even though the
                    // selected outer parameter can type it again from a result equality (an
                    // invariant occurrence or a nested provider-flexible occurrence). Commit that
                    // equality only when every conflicting value argument supports the contextual
                    // result, and never over fixed receiver/context evidence.
                    let fixed = crate::symbol_resolver::expected_result_equality_formals(
                        &source,
                        &result_signature,
                        expected,
                        &result_bindings,
                    );
                    for formal in fixed {
                        if nullable_flexible_roots.contains(formal.as_str()) {
                            continue;
                        }
                        let Some(&fixed_ty) = result_bindings.get(formal.as_str()) else {
                            continue;
                        };
                        if bindings.get(formal.as_str()).copied() == Some(fixed_ty)
                            || non_value_argument_bindings
                                .get(formal.as_str())
                                .is_some_and(|&ty| ty != fixed_ty)
                        {
                            continue;
                        }
                        let mut trial = bindings.clone();
                        trial.insert(crate::types::TyParamName::new(&formal), fixed_ty);
                        if !crate::symbol_resolver::generic_bindings_satisfy_bounds(
                            &signature,
                            &trial,
                            |actual, bound| {
                                self.generic_bound_admits_with_flow_intersection(
                                    actual,
                                    bound,
                                    &flow_intersections,
                                )
                            },
                        ) {
                            continue;
                        }
                        let formal_slice = std::slice::from_ref(&formal);
                        let arguments_admit =
                            parameters
                                .iter()
                                .enumerate()
                                .all(|(argument, &visible_parameter)| {
                                    let Some(&declared) =
                                        initial_shape.params.get(visible_parameter)
                                    else {
                                        return false;
                                    };
                                    if !ty_mentions_param(declared, formal_slice) {
                                        return true;
                                    }
                                    let mut parameter =
                                        crate::symbol_resolver::ty_subst_keep_unbound(
                                            declared, &trial,
                                        );
                                    let whole_array =
                                        whole_arrays.get(argument).copied().unwrap_or(false)
                                            || self.file.is_spread_arg(args[argument]);
                                    if initial_shape.call_sig.vararg_index
                                        == Some(visible_parameter)
                                        && !whole_array
                                    {
                                        parameter =
                                            parameter.array_read_elem().unwrap_or(parameter);
                                    }
                                    let actual = arg_tys[argument];
                                    self.receiver_is_assignable(actual, parameter)
                                        || call_arg_kind(self.file, args[argument], actual)
                                            .adapts_integer_literal_to(parameter)
                                        || self.call_result_can_bind_expected(
                                            args[argument],
                                            parameter,
                                        )
                                });
                        if arguments_admit {
                            bindings = trial;
                        }
                    }
                    crate::symbol_resolver::merge_generic_upper_bindings(
                        &signature,
                        type_args,
                        &mut bindings,
                        result_bindings,
                        |actual, upper| self.receiver_is_assignable(actual, upper),
                    );
                }
            }
            crate::symbol_resolver::publish_denotable_upper_bindings(
                &mut bindings,
                denotable_upper_bindings,
                expected_return_intersection_bindings.as_ref(),
                |sub, sup| self.receiver_is_assignable(sub, sup),
            );
            // Join bottom bindings against `where`-clause subtype constraints IN the real
            // bindings — the return type substitutes from them (`ifBlank { null }` must select
            // with `R = String?`, not pass a check-local copy and return `Nothing?`).
            crate::symbol_resolver::complete_bottom_constraint_bindings(
                &signature,
                &mut bindings,
                type_args,
            );
            // An argument can pin a variable to a type its OWN bound forbids (`f(Auth(),
            // listOf(Login()))` against `<T : Base<T>, C : T>` pins `T = Login`, and `Login` is a
            // `Base<Cmd>`). The recursive bound is what says where to look for the type the call
            // actually means; without this the candidate is dropped as violating its own bounds.
            crate::symbol_resolver::resolve_bound_violating_bindings(
                Some(&source),
                &signature,
                &mut bindings,
                type_args,
            );
            crate::symbol_resolver::complete_dependency_instantiated_bound_bindings(
                &signature,
                &mut bindings,
                type_args,
            );
            let only_input_actuals = initial_shape
                .context_actual_types
                .iter()
                .enumerate()
                .filter_map(|(parameter, actual)| {
                    actual.map(|actual| (parameter, CallArgKind::Typed(actual), true))
                })
                .chain(
                    parameters
                        .iter()
                        .copied()
                        .zip(argument_kinds.iter().cloned())
                        .zip(whole_arrays.iter().copied())
                        .filter_map(|((visible_parameter, argument), whole_array)| {
                            let parameter =
                                *initial_shape.parameter_indices.get(visible_parameter)?;
                            if !candidate
                                .call_sig
                                .parameter_contributes_to_inference(parameter)
                            {
                                return None;
                            }
                            Some((parameter, argument, whole_array))
                        }),
                )
                .collect::<Vec<_>>();
            if !crate::symbol_resolver::apply_only_input_type_bindings(
                crate::symbol_resolver::OnlyInputCall {
                    source: &source,
                    signature: &result_signature,
                    receiver: candidate_receiver,
                    arguments: &only_input_actuals,
                    vararg_index: candidate.call_sig.vararg_index,
                    expected_result: expected,
                },
                &call_variables.instantiate_formals(&candidate.call_sig.only_input_type_formals),
                type_args,
                &mut bindings,
            ) {
                crate::trace_compiler!(
                    "resolve",
                    "candidate {} rejected: @OnlyInputTypes has no proper input solution bindings={bindings:?} arguments={argument_kinds:?}",
                    candidate.callable.name,
                );
                continue;
            }
            if !crate::symbol_resolver::generic_bindings_admit_expected_return_intersection(
                &result_signature,
                &bindings,
                expected_return_intersection_bindings.as_ref(),
                |actual, bound| {
                    self.generic_bound_admits_with_flow_intersection(
                        actual,
                        bound,
                        &flow_intersections,
                    )
                },
            ) {
                crate::trace_compiler!(
                    "resolve",
                    "candidate {} rejected: inferred type arguments violate declared bounds bindings={bindings:?}",
                    candidate.callable.name,
                );
                continue;
            }
            // A star-projected dispatch receiver can be the sole evidence for a return-only method
            // formal (`Entity<*>.value<T : S>(): T`). Its non-denotable capture has already passed
            // applicability as an open bound; now approximate it for the selected result and FIR
            // substitution. Ordinary unconstrained return-only formals remain untouched.
            if candidate.kind == crate::libraries::FnKind::Member {
                if let Some(receiver) = candidate_receiver {
                    let member = candidate.member_with_return(candidate.callable.ret);
                    let owner_bindings = self.member_receiver_type_bindings(receiver, &member);
                    crate::symbol_resolver::complete_return_only_captured_receiver_bindings(
                        &signature,
                        &owner_bindings,
                        &mut bindings,
                        type_args,
                    );
                }
            }
            crate::trace_compiler!(
                "expected_call",
                "candidate {} inferred bindings={bindings:?} receiver_bindings={receiver_bindings:?} lower_inputs={inferred_lower_inputs:?} expected={expected:?} signature_params={:?} arguments={arg_tys:?}",
                candidate.callable.name,
                signature.params,
            );
            let runtime_bindings = |bindings: &crate::symbol_resolver::GSigBinds| {
                bindings
                    .iter()
                    .map(|(formal, &actual)| {
                        let runtime = actual
                            .non_null()
                            .ty_param_name()
                            .and_then(|identity| flow_intersections.get(identity))
                            .and_then(|constituents| constituents.first().copied())
                            .unwrap_or(actual);
                        (*formal, runtime)
                    })
                    .collect::<crate::symbol_resolver::GSigBinds>()
            };
            let mut specialized_bindings = runtime_bindings(&bindings);
            // Bind this callable's inferred formals without erasing type variables owned by the
            // surrounding declaration. In `fun <T> outer(x: Box<T>) = inner(x, object : I<Box<T>> {})`,
            // the callee binds its own `I` to `Box<T>`; a final substitution that erases every unbound
            // name turns that into raw `Box` and makes the already-valid second argument inapplicable.
            let mut params = signature
                .params
                .iter()
                .map(|parameter| {
                    crate::symbol_resolver::instantiate_slot(
                        &source,
                        Some(&signature),
                        *parameter,
                        &specialized_bindings,
                        crate::symbol_resolver::TypePosition::In,
                        crate::symbol_resolver::UnboundSpecialization::Preserve,
                    )
                })
                .collect::<Vec<_>>();
            let exact_arguments_admitted =
                parameters
                    .iter()
                    .enumerate()
                    .all(|(argument, &visible_parameter)| {
                        let Some(&parameter) =
                            initial_shape.parameter_indices.get(visible_parameter)
                        else {
                            return false;
                        };
                        params
                            .get(parameter)
                            .copied()
                            .zip(arg_tys.get(argument).copied())
                            .is_some_and(|(expected, actual)| {
                                candidate
                                    .call_sig
                                    .parameter_admits(parameter, expected, actual)
                            })
                    });
            if !exact_arguments_admitted {
                crate::trace_compiler!(
                    "resolve",
                    "candidate {} rejected: @Exact argument differs from specialized parameter",
                    candidate.callable.name,
                );
                continue;
            }
            let mut shape = if missing_context {
                shape_without_implicit_context(
                    &params,
                    &candidate.call_sig,
                    candidate.context_count,
                )?
            } else {
                self.contextual_call_shape(
                    scope,
                    &params,
                    &candidate.call_sig,
                    candidate.context_count,
                    argument_names,
                )?
            };
            let Some(declared_params) = shaped_declared_parameters(&signature, &shape) else {
                crate::trace_compiler!(
                    "resolve",
                    "candidate {} rejected: call shape names an undeclared parameter",
                    candidate.callable.name,
                );
                continue;
            };
            let Some(mut score) = call_applicability::score_with_declaration(
                self,
                scope,
                &shape.params,
                &shape.call_sig,
                ArgSlots {
                    args,
                    partial_arg_tys: &partial,
                    arg_names: argument_names,
                    trailing_lambda,
                },
                candidate.source_inputs.as_deref().map(|declaration| {
                    call_applicability::DeclarationInputs {
                        declaration,
                        bindings: &specialized_bindings,
                        parameter_indices: &shape.parameter_indices,
                    }
                }),
                Some(call_applicability::DeclaredParameters {
                    params: &declared_params,
                    formals: &signature.formals,
                }),
            ) else {
                crate::trace_compiler!(
                    "resolve",
                    "candidate {} rejected: mapped arguments do not fit params={:?} bindings={bindings:?} args={arg_tys:?} call_sig={:?}",
                    candidate.callable.name,
                    shape.params,
                    shape.call_sig,
                );
                continue;
            };
            if score.uses_sam_conversion() {
                let mut inferred_from_sam = crate::symbol_resolver::GSigBinds::default();
                for (sam, actual) in score.sam_signatures.iter().zip(arg_tys) {
                    let Some(sam) = sam else { continue };
                    crate::symbol_resolver::unify_ty(
                        Ty::fun_with_shape(
                            sam.params.clone(),
                            sam.ret,
                            sam.context_count,
                            sam.has_receiver,
                            sam.suspend,
                        ),
                        *actual,
                        &mut inferred_from_sam,
                    );
                }
                crate::symbol_resolver::merge_generic_bindings(
                    &signature,
                    type_args.len(),
                    &mut bindings,
                    inferred_from_sam,
                );
                if !crate::symbol_resolver::generic_bindings_admit_expected_return_intersection(
                    &result_signature,
                    &bindings,
                    expected_return_intersection_bindings.as_ref(),
                    |actual, bound| {
                        self.generic_bound_admits_with_flow_intersection(
                            actual,
                            bound,
                            &flow_intersections,
                        )
                    },
                ) {
                    continue;
                }
                specialized_bindings = runtime_bindings(&bindings);
                params = signature
                    .params
                    .iter()
                    .map(|parameter| {
                        crate::symbol_resolver::instantiate_slot(
                            &source,
                            Some(&signature),
                            *parameter,
                            &specialized_bindings,
                            crate::symbol_resolver::TypePosition::In,
                            crate::symbol_resolver::UnboundSpecialization::Preserve,
                        )
                    })
                    .collect();
                shape = if missing_context {
                    shape_without_implicit_context(
                        &params,
                        &candidate.call_sig,
                        candidate.context_count,
                    )?
                } else {
                    self.contextual_call_shape(
                        scope,
                        &params,
                        &candidate.call_sig,
                        candidate.context_count,
                        argument_names,
                    )?
                };
                let Some(declared_params) = shaped_declared_parameters(&signature, &shape) else {
                    crate::trace_compiler!(
                        "resolve",
                        "candidate {} rejected: call shape names an undeclared parameter",
                        candidate.callable.name,
                    );
                    continue;
                };
                let Some(specialized_score) = call_applicability::score_with_declaration(
                    self,
                    scope,
                    &shape.params,
                    &shape.call_sig,
                    ArgSlots {
                        args,
                        partial_arg_tys: &partial,
                        arg_names: argument_names,
                        trailing_lambda,
                    },
                    candidate.source_inputs.as_deref().map(|declaration| {
                        call_applicability::DeclarationInputs {
                            declaration,
                            bindings: &specialized_bindings,
                            parameter_indices: &shape.parameter_indices,
                        }
                    }),
                    Some(call_applicability::DeclaredParameters {
                        params: &declared_params,
                        formals: &signature.formals,
                    }),
                ) else {
                    continue;
                };
                score = specialized_score;
                sam_converted.insert(index);
            }
            let Ok(mapped_slots) = map_call_args(
                args,
                argument_names,
                &shape.call_sig.param_names,
                shape.params.len(),
                shape.call_sig.required,
                &shape.call_sig.param_defaults,
                shape.call_sig.vararg_index,
                trailing_lambda,
            ) else {
                continue;
            };
            let omitted_defaults = mapped_slots
                .iter()
                .filter(|argument| argument.is_none())
                .count();
            let argument_parameters = call_argument_parameter_indices(
                args.len(),
                shape.params.len(),
                argument_names,
                trailing_lambda,
                &shape.call_sig,
            )?;
            let named_whole_arrays =
                named_whole_array_varargs(&argument_parameters, argument_names, &shape.call_sig);
            let packed_vararg = shape.call_sig.vararg_index.and_then(|shape_vararg| {
                let declaration_vararg = *shape.parameter_indices.get(shape_vararg)?;
                let mut supplied = false;
                let mut needs_pack = false;
                for (argument, (&parameter, &whole_array)) in argument_parameters
                    .iter()
                    .zip(&named_whole_arrays)
                    .enumerate()
                {
                    if parameter == shape_vararg {
                        supplied = true;
                        needs_pack |= !whole_array || self.file.is_spread_arg(args[argument]);
                    }
                }
                // An omitted vararg is still a selected empty-array pack. A named whole-array
                // binding is the sole pass-through form and therefore needs no packing marker.
                (!supplied || needs_pack).then_some(declaration_vararg)
            });
            let selection_params = argument_parameters
                .iter()
                .copied()
                .zip(named_whole_arrays.iter().copied())
                .enumerate()
                .map(|(argument, (parameter, whole_array))| {
                    let declared = *shape.params.get(parameter)?;
                    if shape.call_sig.vararg_index == Some(parameter)
                        && !whole_array
                        && !self.file.is_spread_arg(args[argument])
                    {
                        declared.array_read_elem().or(Some(declared))
                    } else {
                        Some(declared)
                    }
                })
                .collect::<Option<Vec<_>>>()?;
            // Preserve the declaration-side parameter shapes aligned to SOURCE arguments. Generic
            // inference can make `<T> f(T)` and `<T> f(() -> T)` both specialize to the same lambda
            // type, but Kotlin still chooses the structurally functional declaration. Named/vararg
            // calls need this exact mapping; positional reconstruction would depend on spelling.
            let declaration_specificity_params = argument_parameters
                .iter()
                .copied()
                .zip(named_whole_arrays.iter().copied())
                .enumerate()
                .map(|(argument, (visible_parameter, whole_array))| {
                    let parameter = *shape.parameter_indices.get(visible_parameter)?;
                    let mut declared = *signature.params.get(parameter)?;
                    if shape.call_sig.vararg_index == Some(visible_parameter)
                        && !whole_array
                        && !self.file.is_spread_arg(args[argument])
                    {
                        declared = declared.array_read_elem().unwrap_or(declared);
                    }
                    Some(crate::types::ty_subst(
                        declared,
                        &crate::types::TyParamBindings::default(),
                    ))
                })
                .collect::<Option<Vec<_>>>()?;
            let inferred_ret = crate::symbol_resolver::instantiate_slot(
                &source,
                Some(&signature),
                signature.ret,
                &specialized_bindings,
                crate::symbol_resolver::TypePosition::Out,
                crate::symbol_resolver::UnboundSpecialization::Preserve,
            );
            let specialized_ret = self.contextual_result(call_result_constraint::BottomInputs {
                signature: &signature,
                bindings: &specialized_bindings,
                inferred: inferred_ret,
                constraint: result_constraint,
                receiver: inference_receiver.or(candidate_receiver),
                shape: &shape,
                parameters: &argument_parameters,
                arg_tys,
                whole_arrays: &named_whole_arrays,
                args,
            });
            let ret = candidate.ret.apply(
                candidate
                    .generic_sig
                    .as_ref()
                    .map_or(specialized_ret, |sig| {
                        sig.apply_return_policy(self.libraries, specialized_ret)
                    }),
            );
            let receiver = signature.receiver.map(|receiver| {
                crate::symbol_resolver::ty_subst_keep_unbound(receiver, &specialized_bindings)
            });
            if candidate.is_extension() {
                let receiver = receiver.or(candidate.receiver)?;
                if inference_receiver
                    .or(candidate_receiver)
                    .is_some_and(|actual| !self.receiver_accepts_inference(actual, receiver))
                {
                    continue;
                }
                candidate.receiver = Some(receiver);
                // PHYSICAL parameters: the receiver goes after the leading context parameters, the
                // slot `libraries::extension_receiver_index` names.
                let mut physical = params.clone();
                physical.insert(candidate.context_count.min(physical.len()), receiver);
                candidate.callable.params = physical;
            } else {
                candidate.callable.params = params.clone();
            }
            candidate.callable.ret = ret;
            if let Some(vararg) = packed_vararg {
                candidate.callable.vararg_elem =
                    params.get(vararg).and_then(|ty| ty.array_read_elem());
                candidate.callable.vararg_index = candidate.callable.vararg_elem.map(|_| vararg);
            } else {
                candidate.callable.vararg_elem = None;
                candidate.callable.vararg_index = None;
            }
            let generic = !signature.formals.is_empty();
            let mut intersection_bindings = bindings
                .iter()
                .filter_map(|(formal, actual)| {
                    let identity = actual.non_null().ty_param_name()?;
                    flow_intersections
                        .get(identity)
                        .cloned()
                        .map(|bounds| (formal.to_string(), bounds))
                })
                .collect::<HashMap<_, _>>();
            for (formal, inputs) in &inferred_lower_inputs {
                let Some(formal_index) = signature
                    .formals
                    .iter()
                    .position(|candidate| candidate == formal)
                else {
                    continue;
                };
                if type_args
                    .get(formal_index)
                    .is_some_and(|argument| *argument != Ty::Error)
                    || intersection_bindings.contains_key(formal.as_str())
                    || inputs.len() < 2
                    || inputs
                        .iter()
                        .any(|input| input.non_null().kotlin_class_internal().is_none())
                {
                    continue;
                }
                let views = self.minimal_common_classifier_views_many(inputs);
                if views.len() > 1 {
                    intersection_bindings.insert(formal.clone(), views);
                }
            }
            // A literal is built from the classifier its type variable's OTHER bounds name: one
            // such classifier builds it, and two leave its type ambiguous (a lower bound
            // `MutableSet<E>` and an expected upper `Set<*>` both declare a factory).
            let ambiguous_literals = inference_actuals
                .iter()
                .filter_map(|&(argument, parameter, _, whole_array)| {
                    if whole_array
                        || !matches!(
                            argument_kinds[argument],
                            CallArgKind::CollectionLiteral { .. }
                        )
                    {
                        return None;
                    }
                    let mut declared = signature.params[parameter];
                    if candidate.call_sig.vararg_index == Some(parameter) {
                        declared = declared.array_read_elem().unwrap_or(declared);
                    }
                    let (TyKind::TyParam(formal, _) | TyKind::Var(formal, _)) =
                        declared.non_null().kind()
                    else {
                        return None;
                    };
                    let lowers = preliminary
                        .lower_inputs
                        .get(formal.as_str())
                        .map_or(&[][..], Vec::as_slice);
                    let upper = expected_literal_targets
                        .as_ref()
                        .and_then(|targets| targets.get(formal.as_str()))
                        .copied();
                    let classifiers = self.literal_bound_classifiers(lowers, upper);
                    (!classifiers.is_empty()).then_some((argument, classifiers))
                })
                .collect::<Vec<_>>();
            let variables = call_variables.variables();
            let selected = call_site_publication::declared_selection(
                variables,
                candidate,
                specialized_bindings,
                intersection_bindings,
                index,
                ambiguous_literals,
            );
            let shapes = call_site_publication::CandidateParameterShapes {
                selection: selection_params,
                declaration_specificity: declaration_specificity_params,
            }
            .declared(variables);
            let (selection_params, declaration_specificity_params) =
                (shapes.selection, shapes.declaration_specificity);
            if !score.lambda_result_fits {
                misfits.insert(index);
            }
            applicable.push((
                score.rank,
                generic,
                index,
                missing_context,
                omitted_defaults,
                selected,
                selection_params,
                declaration_specificity_params,
            ));
            crate::trace_compiler!(
                "candidate_select",
                "call={} candidate={}.{} supplied={:?} omitted={} rank={:?} default_realization={} descriptor={}",
                call.0,
                applicable.last().unwrap().5.callable.owner,
                applicable.last().unwrap().5.callable.name,
                applicable.last().unwrap().6,
                omitted_defaults,
                score.rank,
                applicable.last().unwrap().5.callable.default_realization.is_some(),
                applicable.last().unwrap().5.callable.descriptor,
            );
            crate::trace_compiler!(
                "resolve",
                "candidate accepted rank={:?} generic={generic} receiver_rank={} missing_context={missing_context} bindings={:?} inferred_ret={:?} specialized_ret={:?} return_info=({:?},{:?}) ret={:?}",
                score.rank,
                applicable.last().unwrap().5.receiver_rank,
                applicable.last().unwrap().5.bindings,
                inferred_ret,
                specialized_ret,
                applicable.last().unwrap().5.ret.nullable,
                applicable.last().unwrap().5.ret.class,
                applicable.last().unwrap().5.callable.ret,
            );
        }
        // kotlinc ranks a visibility error below every resolved candidate, on any tower rung: an
        // invisible candidate leaves the call to a visible one and is reported, or silenced by a
        // `@Suppress`, only when nothing visible applies. The invisible candidates then compete as
        // one group across scope levels (see `site_tower_rank`).
        if applicable.iter().any(|(_, _, _, _, _, candidate, _, _)| {
            self.candidate_visible(candidate, extension_receiver)
        }) {
            applicable.retain(|(_, _, _, _, _, candidate, _, _)| {
                self.candidate_visible(candidate, extension_receiver)
            });
        }
        // Applicability is evaluated one callable-tower rung at a time. A declaration from a
        // farther import/lexical rung must not beat an applicable nearer extension merely because
        // its receiver or value parameters are more specific. Likewise, receiver distance only
        // orders candidates *inside* one extension scope rung. Keep `HidesMembers` above members,
        // members above ordinary extensions, and skip a rung whose only candidates lack context.
        // Later rules share one rung. Argument score there is type fit and omitted defaults;
        // a vararg loses only after element specificity, as a tie-break among equal shapes.
        let tower_rank = |candidate: &SelectedCallable| {
            crate::symbol_resolver::site_tower_rank(
                crate::symbol_resolver::callable_tower_rank(candidate),
                self.candidate_visible(candidate, extension_receiver),
            )
        };
        let nearest_available_rung = applicable
            .iter()
            .filter(|(_, _, _, missing_context, ..)| !missing_context)
            .map(|(_, _, _, _, _, candidate, _, _)| tower_rank(candidate))
            .min()
            .or_else(|| {
                applicable
                    .iter()
                    .map(|(_, _, _, _, _, candidate, _, _)| tower_rank(candidate))
                    .min()
            })?;
        applicable.retain(|(_, _, _, _, _, candidate, _, _)| {
            tower_rank(candidate) == nearest_available_rung
        });
        let low_priority_annotation =
            crate::types::type_name("kotlin/internal/LowPriorityInOverloadResolution");
        if applicable.iter().any(|(_, _, _, _, _, candidate, _, _)| {
            !candidate
                .callable
                .annotations
                .contains(&low_priority_annotation)
        }) {
            applicable.retain(|(_, _, _, _, _, candidate, _, _)| {
                !candidate
                    .callable
                    .annotations
                    .contains(&low_priority_annotation)
            });
        }
        call_applicability::retain_eager_fits(self.file, &mut applicable, |c| {
            !misfits.contains(&c.2)
        });
        let has_context = applicable
            .iter()
            .any(|(_, _, _, missing_context, _, _, _, _)| !missing_context);
        if has_context {
            applicable.retain(|(_, _, _, missing_context, _, _, _, _)| !missing_context);
        }
        if applicable
            .iter()
            .any(|(_, _, index, ..)| !sam_converted.contains(index))
        {
            applicable.retain(|(_, _, index, ..)| !sam_converted.contains(index));
        }
        if applicable.iter().any(|(_, _, _, _, _, candidate, _, _)| {
            candidate.kind == crate::libraries::FnKind::Member
        }) {
            // A conversion-free candidate beats one that needs lambda-to-SAM adaptation. Among
            // candidates with the same adaptation cost, an applicable member keeps receiver-tower
            // precedence over extensions.
            applicable.retain(|(_, _, _, _, _, candidate, _, _)| {
                candidate.kind == crate::libraries::FnKind::Member
            });
        }
        let best = applicable
            .iter()
            .filter(|(_, _, _, missing_context, _, _, _, _)| !has_context || !*missing_context)
            .map(|(rank, ..)| rank.0)
            .max()?;
        let nearest_receiver = applicable
            .iter()
            .filter(|(rank, _, _, missing_context, _, _, _, _)| {
                rank.0 == best && (!has_context || !missing_context)
            })
            .map(|(_, _, _, _, _, candidate, _, _)| candidate.receiver_rank)
            .min()?;
        let selecting_extension = applicable
            .iter()
            .all(|(_, _, _, _, _, candidate, _, _)| candidate.is_extension());
        let mut maximal = applicable
            .into_iter()
            .filter(|(rank, _, _, missing_context, _, candidate, _, _)| {
                rank.0 == best
                    && (!has_context || !missing_context)
                    && candidate.receiver_rank == nearest_receiver
            })
            .collect::<Vec<_>>();
        let declared_receiver = |candidate: &SelectedCallable| {
            candidate
                .is_extension()
                .then(|| candidate.semantic_receiver())
                .flatten()
                .map(|receiver| {
                    crate::types::ty_subst(receiver, &crate::types::TyParamBindings::default())
                })
        };
        if maximal.len() > 1 {
            // Fewer omitted defaults wins only between equally specific declarations.
            let source = self.fed_source();
            let dominated = maximal
                .iter()
                .enumerate()
                .filter_map(|(index, (_, _, _, _, omitted, candidate, _, declared))| {
                    maximal
                        .iter()
                        .any(|(_, _, _, _, other_omitted, other, _, other_declared)| {
                            other_omitted < omitted
                                && crate::symbol_resolver::declarations_equally_specific(
                                    &source,
                                    (declared_receiver(candidate), declared),
                                    (declared_receiver(other), other_declared),
                                )
                        })
                        .then_some(index)
                })
                .collect::<crate::fx_hash::HashSet<_>>();
            maximal = maximal
                .into_iter()
                .enumerate()
                .filter_map(|(index, candidate)| (!dominated.contains(&index)).then_some(candidate))
                .collect();
        }
        if maximal.len() > 1 {
            let dominated = maximal
                .iter()
                .enumerate()
                .filter_map(|(index, (_, _, _, _, _, candidate, _, _))| {
                    let signature = candidate.info.generic_sig.as_ref()?;
                    maximal
                        .iter()
                        .any(|(_, _, _, _, _, other, _, _)| {
                            other.info.generic_sig.as_ref().is_some_and(|other| {
                                crate::symbol_resolver::generic_signature_strictly_more_constrained(
                                    &self.fed_source(),
                                    other,
                                    signature,
                                )
                            })
                        })
                        .then_some(index)
                })
                .collect::<crate::fx_hash::HashSet<_>>();
            maximal = maximal
                .into_iter()
                .enumerate()
                .filter_map(|(index, candidate)| (!dominated.contains(&index)).then_some(candidate))
                .collect();
        }
        crate::symbol_resolver::retain_most_specific_declarations(
            &self.fed_source(),
            extension_receiver,
            &mut maximal,
            |(_, generic, _, _, _, candidate, _, parameters)| {
                let receiver = declared_receiver(candidate);
                let member_owner = (candidate.kind == crate::libraries::FnKind::Member)
                    .then_some(candidate.callable.owner);
                (receiver, parameters, *generic, member_owner)
            },
        );
        call_applicability::refine_lambda_tie(self.file, (args, arg_tys), &mut maximal, |t| {
            (&t.5.info, &t.6, !misfits.contains(&t.2))
        });
        if maximal.len() > 1 {
            // A generic declaration whose parameters another's accept, and not the reverse, is the
            // more general one: `reference(Column<T>)` over a free `T` accepts
            // `reference(Column<E>)` over `E : EntityID<T>`, which is the more specific. A vararg
            // takes part through the element its one argument supplies, as in the signature phase.
            let source = self.fed_source();
            let shapes = maximal
                .iter()
                .map(|(_, _, _, _, _, candidate, _, _)| {
                    candidate.info.generic_sig.as_ref().and_then(|_| {
                        crate::symbol_resolver::declared_call_shape(&candidate.info, arg_tys.len())
                    })
                })
                .collect::<Vec<_>>();
            let retained = crate::symbol_resolver::declaration_dominance_maxima(&source, &shapes)
                .into_iter()
                .collect::<crate::fx_hash::HashSet<_>>();
            maximal = maximal
                .into_iter()
                .enumerate()
                .filter_map(|(index, candidate)| retained.contains(&index).then_some(candidate))
                .collect();
        }
        if maximal.len() > 1 {
            // Generic overloads of which neither accepts the other's parameters are incomparable:
            // kotlinc ranks declarations by exactly that solve, so they are ambiguous. Only the
            // overloads that take the arguments with their type parameters at their bounds take
            // part: an argument whose type is not final yet is left to the ordinary selection.
            let source = self.fed_source();
            let no_bindings = crate::types::TyParamBindings::default();
            let viable = maximal
                .iter()
                .filter_map(|entry| {
                    let candidate = &entry.5;
                    candidate.info.generic_sig.as_ref()?;
                    let shape = crate::symbol_resolver::declared_call_shape(
                        &candidate.info,
                        arg_tys.len(),
                    )?;
                    (shape.params.len() == arg_tys.len()
                        && shape
                            .params
                            .iter()
                            .zip(arg_tys)
                            .all(|(parameter, argument)| {
                                crate::symbol_resolver::resolution_subtype(
                                    &source,
                                    *argument,
                                    parameter.substitute_erased(&no_bindings),
                                )
                            }))
                    .then_some((entry, shape))
                })
                .collect::<Vec<_>>();
            // Only a parameter that mentions a type parameter is compared by the solve; a
            // constructor's class parameters do not make `(Int)` and `(Long)` incomparable.
            let compared = viable.iter().all(|(_, shape)| {
                shape
                    .params
                    .iter()
                    .any(|parameter| parameter.mentions_ty_param())
            });
            if viable.len() > 1
                && compared
                && viable.iter().enumerate().all(|(index, (_, left))| {
                    viable.iter().skip(index + 1).all(|(_, right)| {
                        crate::symbol_resolver::declarations_incomparable(&source, left, right)
                    })
                })
            {
                return Some(CallableCandidateSelection::Ambiguous(
                    viable
                        .iter()
                        .map(|(entry, _)| entry.5.info.clone())
                        .collect(),
                    viable
                        .iter()
                        .filter_map(|(entry, _)| entry.5.candidate_index)
                        .collect(),
                ));
            }
        }
        // Distinct SAM target types do not make an overload family inherently ambiguous. The
        // ordinary most-specific relation below must still compare them: if one SAM interface
        // inherits the other, its overload wins (`MyRunnable : Runnable`; `foo(MyRunnable)` beats
        // `foo(Runnable)` for a lambda). Unrelated SAM targets remain incomparable and are reported
        // ambiguous by that same selector, so no wrapper realization is chosen by provider order.
        let selected_index = if maximal.len() == 1 {
            0
        } else {
            let ranked = maximal
                .iter()
                .map(|(_, _, _, _, _, candidate, params, _)| {
                    let params = selecting_extension
                        .then_some(extension_receiver)
                        .flatten()
                        .and(candidate.receiver)
                        .into_iter()
                        .chain(params.iter().copied())
                        .collect();
                    (&candidate.info, params)
                })
                .collect::<Vec<_>>();
            let source = self.fed_source();
            let argument_kinds = selecting_extension
                .then_some(extension_receiver)
                .flatten()
                .map(crate::symbol_resolver::CallArgKind::Typed)
                .into_iter()
                .chain(args.iter().map(|&argument| {
                    // Candidate applicability above has already checked each lambda's body result
                    // against its own expected return. For the final most-specific comparison,
                    // preserve the one remaining piece of postponed source syntax: an omitted
                    // parameter list that does not bind `it` can denote either Function0 or a
                    // one-input function with implicit `it`. Replacing it with the expectation-free
                    // fallback Function0 here would discard every applicable one-input SAM and turn
                    // a real ambiguity into “none applicable.”
                    if self.lambda_probe_ty(argument) == Some(Ty::Error) {
                        CallArgKind::LambdaLiteral(Ty::Error)
                    } else if let Some(intersection) =
                        self.flow_intersection_argument(scope, argument)
                    {
                        // A stable path proven to be several types is applicable to a parameter
                        // of any one of them, as it was when the candidate was admitted.
                        CallArgKind::Typed(intersection)
                    } else {
                        self.call_arg_kind(scope, argument)
                    }
                }))
                .collect::<Vec<_>>();
            let declaration_selected = argument_kinds
                .iter()
                .any(|argument| {
                    matches!(argument, CallArgKind::LambdaLiteral(ty) if *ty == Ty::Error)
                })
                .then(|| {
                    let shapes = maximal
                        .iter()
                        .map(|(_, _, _, _, _, candidate, _, declaration_params)| {
                            selecting_extension
                                .then(|| declared_receiver(candidate))
                                .flatten()
                                .into_iter()
                                .chain(declaration_params.iter().copied())
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>();
                    crate::symbol_resolver::most_specific_parameter_shape_index(
                        &source,
                        &shapes,
                        &argument_kinds,
                    )
                })
                .and_then(|selected| match selected {
                    crate::symbol_resolver::CandidateSelection::Selected(index) => Some(index),
                    crate::symbol_resolver::CandidateSelection::None
                    | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
                });
            if let Some(selected) = declaration_selected {
                selected
            } else {
                match crate::symbol_resolver::best_by_args(
                    self.libraries,
                    &source,
                    &ranked,
                    &argument_kinds,
                ) {
                    crate::symbol_resolver::CandidateSelection::Selected(selected) => maximal
                        .iter()
                        .position(|(_, _, _, _, _, candidate, _, _)| {
                            std::ptr::eq(&candidate.info, selected)
                        })?,
                    crate::symbol_resolver::CandidateSelection::Ambiguous => {
                        return Some(CallableCandidateSelection::Ambiguous(
                            maximal
                                .iter()
                                .map(|(_, _, _, _, _, candidate, _, _)| candidate.info.clone())
                                .collect(),
                            maximal.iter().filter_map(|t| t.5.candidate_index).collect(),
                        ));
                    }
                    crate::symbol_resolver::CandidateSelection::None => return None,
                }
            }
        };
        let (_, _, _, missing_context, _, selected, _, _) =
            maximal.into_iter().nth(selected_index)?;
        crate::trace_compiler!(
            "candidate_select",
            "call={} selected={}.{} params={:?} default_realization={} descriptor={}",
            call.0,
            selected.callable.owner,
            selected.callable.name,
            selected.callable.params,
            selected.callable.default_realization.is_some(),
            selected.callable.descriptor,
        );
        // Each commit of the call replaces what an earlier commit published for its literals.
        let mut ambiguities = self.collection_literal_ambiguities.borrow_mut();
        for argument in args {
            ambiguities.remove(argument);
        }
        for (argument, classifiers) in &selected.ambiguous_literals {
            ambiguities.insert(args[*argument], classifiers.clone());
        }
        drop(ambiguities);
        Some(if missing_context {
            CallableCandidateSelection::MissingContext(Box::new(selected))
        } else {
            CallableCandidateSelection::Selected(Box::new(selected))
        })
    }
}
