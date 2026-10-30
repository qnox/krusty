//! Provider constructor selection through the common argument mapper and constraint solver.

use super::*;

/// What a selection over a wider candidate family already decided for a constructor call.
#[derive(Clone, Copy)]
pub(super) enum ConstructorPreselection<'a> {
    /// Nothing: the constructor family is selected here.
    Open,
    /// One declaration was chosen; only it is materialized.
    Selected(&'a crate::libraries::FunctionInfo),
    /// The candidates are equally specific: report why, but commit none.
    Ambiguous,
    /// No candidate applies: commit none, whatever survives the filters here. The declaration
    /// the arguments map onto only records the type arguments it infers, and the rejection is
    /// left unreported for the shared selector's caller to report.
    Rejected,
}

impl Checker<'_> {
    /// The constructor path's view of one checked argument expression: its call-argument kind,
    /// with an integer literal typed by its semantic (not runtime) type.
    fn constructor_argument_kind(&self, argument: ExprId) -> CallArgKind {
        let nominal = self.expr_types[argument.0 as usize];
        match call_arg_kind(self.file, argument, nominal) {
            CallArgKind::IntegerLiteral { ty, constant } => {
                CallArgKind::integer_constant(self.integer_literal_semantic_type(ty), constant)
            }
            kind => kind,
        }
    }

    /// The inference input `kind` contributes to `declared`: an integer literal adapts to the
    /// first scalar bound along the declared type-parameter chain that can hold it.
    fn constructor_inference_input(&self, kind: &CallArgKind, declared: Ty) -> Ty {
        let mut bounded = declared.non_null();
        while let TyKind::TyParam(_, bound) = bounded.kind() {
            let value_bound = self.integer_literal_semantic_type(bound);
            if kind.adapts_integer_literal_to(value_bound) {
                return value_bound;
            }
            bounded = bound.non_null();
        }
        kind.ty()
    }

    /// Select one provider constructor tier through the same argument mapper and specificity
    /// operation used by source primary/secondary constructors. Every provider declaration already
    /// owns its source call shape in `LibraryMember::call_sig`; no detached parameter inventory or
    /// origin-specific applicability logic participates in selection.
    pub(super) fn record_library_constructor(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        internal: TypeName,
        classifier: &crate::libraries::LibraryType,
        args: &[ExprId],
        options: LibraryConstructorOptions<'_>,
    ) -> Result<LibraryConstructorSelection, LibraryConstructorFailure> {
        let LibraryConstructorOptions {
            arg_names,
            applied_classifier,
            bound_outer,
            expected,
            priority,
            selected: preselection,
        } = options;
        let bound_context = match bound_outer {
            Some(receiver) => {
                let source = self.fed_source();
                let Some(context) = crate::symbol_resolver::BoundInnerConstructorContext::new(
                    &source, receiver, classifier,
                ) else {
                    return Ok(LibraryConstructorSelection::NoMatch);
                };
                Some(context)
            }
            None => None,
        };
        self.contextualize_constructor_arrays(scope, call, internal, args, arg_names);
        for &argument in args {
            if self.expr_types[argument.0 as usize] == Ty::Error
                && !self.file.is_callable_syntax(argument)
            {
                self.expr(scope, argument);
            }
        }
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        let names = arg_names
            .map(<[Option<String>]>::to_vec)
            .unwrap_or_else(|| vec![None; args.len()]);
        let arguments = CtorDelegationCall {
            args: args.to_vec(),
            names,
            trailing_lambda,
        };
        let applied_classifier = self.alias_constructor_fixed_application(call, applied_classifier);
        let explicit_type_arguments = applied_classifier
            .filter(|applied| {
                applied.kotlin_class_internal() == Some(internal)
                    && applied.type_args().len() == classifier.type_params().len()
            })
            .map(|applied| applied.type_args().to_vec())
            .unwrap_or_else(|| self.resolved_explicit_type_args(scope, call));
        let expected_classifier =
            Self::alias_constructor_result_constraint(applied_classifier, expected);
        let mut members = Vec::new();
        let mut member_bindings = Vec::new();
        let mut candidates = Vec::new();
        let mut mapping_failures = Vec::new();
        let mut bound_failures = Vec::new();
        let mut type_argument_bound_failure = None;

        let mut declarations = self.constructor_declarations(internal, classifier);
        if let ConstructorPreselection::Selected(selected) = preselection {
            declarations.retain(|declaration| {
                declaration.stable_declaration == selected.stable_declaration
                    && declaration.descriptor == selected.callable.descriptor
            });
        }
        let low_priority_annotation =
            crate::types::type_name("kotlin/internal/LowPriorityInOverloadResolution");
        declarations.retain(|constructor| {
            let low = constructor.annotations.contains(&low_priority_annotation);
            match priority {
                ConstructorPriorityTier::All => true,
                ConstructorPriorityTier::Ordinary => !low,
                ConstructorPriorityTier::Low => low,
            }
        });
        for declaration in &mut declarations {
            if let Some(context) = &bound_context {
                let source = self.fed_source();
                let candidate = context
                    .candidate(
                        &source,
                        bound_outer.expect("a bound context has an enclosing receiver"),
                        internal,
                        classifier,
                        declaration.clone(),
                    )
                    .expect("a bound inner context has an inner constructor candidate");
                *declaration = candidate.member_with_return(candidate.semantic_signature().ret);
            }
            declaration.call_sig = Self::normalized_constructor_call_sig(declaration);
        }
        let partial = args
            .iter()
            .map(|&argument| {
                (!self.file.is_callable_syntax(argument))
                    .then(|| self.expr_types[argument.0 as usize])
            })
            .collect::<Vec<_>>();
        let declarations = self.eager_lambda_constructors(
            scope,
            eager_lambda_analysis::EagerLambdaCall {
                shape: UntypedLambdaCall {
                    args,
                    partial: &partial,
                    arg_names,
                    trailing_lambda,
                    type_args: &explicit_type_arguments,
                    expected,
                },
                label: call_implicit_lambda_label(self.file, call),
            },
            declarations,
        );

        for declaration in declarations {
            let mut member = declaration;
            if bound_context.is_none() {
                member.generic_sig =
                    crate::libraries::constructor_generic_signature(internal, classifier, &member);
            }
            let declaration_parameter_shapes = member.constructor_declaration_params().to_vec();
            let parameter_count = member.params.len();
            let call_sig = Self::normalized_constructor_call_sig(&member);
            let contextual = match self.contextual_call_shape_result(
                scope,
                &member.params,
                &call_sig,
                member.context_count,
                arg_names,
            ) {
                Ok(contextual) => contextual,
                Err(_) => continue,
            };
            let visible_slots = match call_argument_parameter_indices_result(
                args.len(),
                contextual.params.len(),
                arg_names,
                trailing_lambda,
                &contextual.call_sig,
            ) {
                Ok(slots) => slots,
                Err(failure) => {
                    mapping_failures.push((failure, (member.params.clone(), call_sig.clone())));
                    continue;
                }
            };
            let argument_slots = visible_slots
                .into_iter()
                .map(|slot| contextual.parameter_indices[slot])
                .collect::<Vec<_>>();
            let mut inferred = None;
            if let Some(signature) = member
                .generic_sig
                .as_ref()
                .filter(|signature| signature.params.len() == parameter_count)
            {
                // Explicit type arguments are fixed before any argument is inferred: one outside
                // its bound rejects the constructor whatever its arguments are (`Box<String>("")`).
                if let Some(failure) = self.explicit_constructor_bound_violation(
                    &signature.formals,
                    signature,
                    &explicit_type_arguments,
                ) {
                    type_argument_bound_failure.get_or_insert_with(|| {
                        (
                            failure,
                            Self::generic_constructor_display(signature, &member),
                            bound_context.as_ref().map(|context| {
                                library_constructor_failure::BoundConstructorDiagnostic::new(
                                    context.outer(),
                                    signature.clone(),
                                )
                            }),
                            self.rejected_constructor_lambdas(
                                args,
                                &argument_slots,
                                signature,
                                &explicit_type_arguments,
                            ),
                        )
                    });
                    continue;
                }
                let expected_arguments = expected_classifier
                    .map(Ty::non_null)
                    .filter(|expected| expected.obj_internal() == Some(internal))
                    .map(Ty::type_args)
                    .unwrap_or_default();
                let dnn_null_formal = args
                    .iter()
                    .copied()
                    .zip(argument_slots.iter().copied())
                    .find_map(|(argument, parameter)| {
                        if !Self::null_only_type(self.expr_types[argument.0 as usize]) {
                            return None;
                        }
                        Self::dnn_inference_formal_for_parameter(
                            signature,
                            parameter,
                            |formal_index| {
                                explicit_type_arguments
                                    .get(formal_index)
                                    .or_else(|| expected_arguments.get(formal_index))
                                    .is_some_and(|argument| {
                                        !argument.mentions_error()
                                            && !argument.mentions_pending()
                                            && !argument.is_star_projection()
                                    })
                            },
                        )
                    });
                if let Some(formal) = dnn_null_formal {
                    self.diags.error(
                        self.call_callee_name_span(call),
                        format!(
                            "cannot infer type for type parameter '{formal}'. Specify it explicitly."
                        ),
                    );
                    return Ok(LibraryConstructorSelection::Rejected(Ty::Error));
                }
                let mut enclosing_context_formals = crate::fx_hash::HashSet::default();
                let actuals = args
                    .iter()
                    .zip(&argument_slots)
                    .enumerate()
                    .filter_map(|(source, (&argument, &parameter))| {
                        let expression = self.file.expr(argument);
                        let symbolic = signature.params[parameter];
                        // Lambdas remain postponed until their selected function shape is
                        // known. A callable reference, however, already has an exact nominal
                        // expression type; use it for nominal parameters such as `KClass<T>`.
                        // Function-parameter references still need adaptation and therefore
                        // remain postponed with lambdas.
                        let whole_array = self.file.is_spread_arg(argument)
                            || (call_sig.vararg_index == Some(parameter)
                                && arg_names
                                    .and_then(|names| names.get(source))
                                    .is_some_and(Option::is_some));
                        let checked_lambda = matches!(expression, Expr::Lambda { .. })
                            && self.lambda_parameters_are_contextual(argument)
                            && !self.expr_types[argument.0 as usize].mentions_error()
                            && !self.expr_types[argument.0 as usize].mentions_pending();
                        if (matches!(expression, Expr::Lambda { .. }) && !checked_lambda)
                            || (matches!(expression, Expr::CallableRef { .. })
                                && symbolic.non_null().is_fun())
                        {
                            return None;
                        }
                        let kind = self.constructor_argument_kind(argument);
                        let mut declared = symbolic;
                        if call_sig.vararg_index == Some(parameter) && !whole_array {
                            declared = declared.array_read_elem().unwrap_or(declared);
                        }
                        // A nested generic producer whose result variable is carried by a
                        // lambda input has only a provisional argument type at this point. The
                        // enclosing constructor parameter is part of the same constraint
                        // system: freezing that probe would make `Holder(build { set(X()) })`
                        // infer `Holder<X>` even when `Holder<T : Bound>` must complete the
                        // postponed builder as `Buildee<Bound>`. Provider-backed constructors
                        // are the Pass-2 view of the same source declaration, so they must defer
                        // this evidence exactly like the direct source-constructor path.
                        let postponed = self.call_result_has_postponed_input_formal(argument)
                            || self.unbound_contextual_result_signature(argument).is_some();
                        if postponed {
                            let mut carries_enclosing_context = false;
                            for formal in &signature.formals {
                                if ty_mentions_param(declared, std::slice::from_ref(formal)) {
                                    enclosing_context_formals.insert(formal.clone());
                                    carries_enclosing_context = true;
                                }
                            }
                            if carries_enclosing_context {
                                return None;
                            }
                        }
                        let actual = self.constructor_inference_input(&kind, declared);
                        Some((source, parameter, actual, whole_array))
                    })
                    .collect::<Vec<_>>();
                // Inside the constructed class (or an inner class that sees its formals), the
                // class's own `T` is a fixed type, while this call infers a fresh `T`.
                let lexical = scope.lexical_tparam_identities();
                let source = self.fed_source();
                let inference_actuals = actuals
                    .iter()
                    .flat_map(|&(source_argument, parameter, actual, whole_array)| {
                        // `actual` has already applied source argument semantics such as
                        // integer-literal adaptation to a final scalar bound. A conditional's
                        // branches get the same view one by one, so re-reading their nominal
                        // expression slots cannot undo that decision (`R<T: Long>(if (c) 0 else 1)`).
                        let inputs = match conditional_branch::conditional_argument_branches(
                            self.file,
                            args[source_argument],
                        ) {
                            Some(branches) if !whole_array => {
                                let mut declared = signature.params[parameter];
                                if call_sig.vararg_index == Some(parameter) {
                                    declared = declared.array_read_elem().unwrap_or(declared);
                                }
                                branches
                                    .into_iter()
                                    .map(|branch| {
                                        self.constructor_inference_input(
                                            &self.constructor_argument_kind(branch),
                                            declared,
                                        )
                                    })
                                    .collect()
                            }
                            _ => vec![actual],
                        };
                        inputs
                            .into_iter()
                            .map(move |input| (source_argument, parameter, input, whole_array))
                    })
                    .collect::<Vec<_>>();
                let constraints = crate::symbol_resolver::CallSiteVariables::solve(
                    signature,
                    |formal| lexical.iter().any(|identity| identity == formal),
                    |signature| {
                        crate::symbol_resolver::infer_generic_call_constraints_with_argument_ordinals(
                            &source,
                            signature,
                            inference_actuals,
                            call_sig.vararg_index,
                        )
                    },
                );
                let owner = super::type_parameter_owners::classifier_type_parameter_owner(
                    &source, internal, classifier,
                );
                if let Some(contradictions) =
                    super::generic_argument_bounds::ArgumentContradictions::new(
                        signature,
                        &constraints.bound_violations,
                        owner,
                        |source_argument| {
                            let argument = *args.get(source_argument)?;
                            let parameter = *argument_slots.get(source_argument)?;
                            Some((argument, *signature.params.get(parameter)?))
                        },
                    )
                {
                    bound_failures.push(LibraryConstructorFailure::ArgumentContradictions(
                        Box::new(contradictions),
                    ));
                    continue;
                }
                let tightest_upper = constraints.tightest_upper_bindings(&source);
                let mut bindings = constructor_commitment::initial_provider_bindings(
                    &source,
                    internal,
                    classifier,
                    expected_classifier,
                    &constraints.bindings,
                    &constraints.upper_only,
                    &tightest_upper,
                );
                if let Some(context) = &bound_context {
                    bindings.extend(context.bindings().clone());
                }
                if let Some(expected) = expected_classifier {
                    if let Some(expected_bindings) =
                        crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
                            &source,
                            signature,
                            expected,
                            |actual, bound| self.receiver_is_assignable(actual, bound),
                        )
                    {
                        self.retain_expected_flexible_constructor_bindings(
                            constructor_commitment::ExpectedConstructorBindingInputs {
                                source: &source,
                                signature,
                                expected,
                                expected_bindings: &expected_bindings,
                                nullable_flexible_roots: &constraints.nullable_flexible_roots,
                                explicit_type_arguments: &explicit_type_arguments,
                                actuals: &actuals,
                                vararg_index: call_sig.vararg_index,
                            },
                            &mut bindings,
                        );
                        // A caller-owned lexical variable cannot ordinarily be constructed from
                        // its upper bound. Integer constants are the exception when contextual
                        // literal typing can publish that exact symbolic type (`T : Int`, then
                        // `IntBox(-1)` in an `IntBox<T>` position). Prove the complete parameter
                        // substitution before committing this equality; an ordinary `Int` value
                        // still cannot masquerade as an arbitrary lexical `T`.
                        let invariant = crate::symbol_resolver::expected_result_equality_formals(
                            &source,
                            signature,
                            expected,
                            &expected_bindings,
                        );
                        for formal in invariant {
                            let Some(index) = signature
                                .formals
                                .iter()
                                .position(|candidate| candidate == &formal)
                            else {
                                continue;
                            };
                            if explicit_type_arguments
                                .get(index)
                                .is_some_and(|argument| *argument != Ty::Error)
                            {
                                continue;
                            }
                            let Some(&fixed) = expected_bindings.get(&formal).filter(|&&fixed| {
                                fixed.non_null().is_ty_param()
                                    && Self::type_is_lexically_fixed(scope, fixed)
                            }) else {
                                continue;
                            };
                            let mut trial = bindings.clone();
                            trial.insert(formal.clone(), fixed);
                            if !crate::symbol_resolver::generic_bindings_satisfy_bounds(
                                signature,
                                &trial,
                                |actual, bound| self.generic_bound_admits(actual, bound),
                            ) {
                                continue;
                            }
                            let mut contributed = false;
                            let arguments_fit = actuals.iter().all(
                                |&(source_argument, parameter_index, _, whole_array)| {
                                    let symbolic = signature.params[parameter_index];
                                    if !ty_mentions_param(symbolic, std::slice::from_ref(&formal)) {
                                        return true;
                                    }
                                    contributed = true;
                                    let mut parameter =
                                        crate::symbol_resolver::ty_subst_keep_unbound(
                                            symbolic, &trial,
                                        );
                                    if call_sig.vararg_index == Some(parameter_index)
                                        && !whole_array
                                    {
                                        parameter =
                                            parameter.array_read_elem().unwrap_or(parameter);
                                    }
                                    let expression = args[source_argument];
                                    let nominal = self.expr_types[expression.0 as usize];
                                    let contextual = self.argument_type_for_expected(
                                        scope, expression, nominal, parameter,
                                    );
                                    self.receiver_is_assignable(contextual, parameter)
                                },
                            );
                            if contributed && arguments_fit {
                                bindings = trial;
                            }
                        }
                        // An invariant constructor result is an equality constraint on its class
                        // variables, but only when every argument still fits after applying that
                        // equality. This is the same bidirectional rule used for ordinary generic
                        // calls: `Box(Concrete)` may become `Box<Base>` in a `Box<Base>` context,
                        // while `Bag(listOf(Other()))` must remain `Bag<Other>` when `List` is
                        // invariant and therefore fail against `Bag<Entry>` at the value boundary.
                        crate::symbol_resolver::widen_invariant_expected_bindings(
                            &source,
                            signature,
                            explicit_type_arguments.as_slice(),
                            &mut bindings,
                            &expected_bindings,
                            expected,
                            |actual, bound| self.receiver_is_assignable(actual, bound),
                        );
                    }
                }
                // Without independent evidence, a constructor variable that supplies context to a
                // postponed producer completes from its declared upper bound. Substitute preceding
                // solutions so dependent bounds (`U : Base<T>`) retain their semantic application.
                for (index, formal) in classifier.type_params().iter().enumerate() {
                    if !enclosing_context_formals.contains(formal)
                        || bindings
                            .get(formal)
                            .is_some_and(|binding| *binding != Ty::Error)
                    {
                        continue;
                    }
                    let known = classifier
                        .type_params()
                        .iter()
                        .take(index)
                        .filter_map(|known_formal| {
                            bindings
                                .get(known_formal)
                                .copied()
                                .filter(|binding| *binding != Ty::Error)
                                .map(|binding| (known_formal.clone(), binding))
                        })
                        .collect::<HashMap<_, _>>();
                    if let Some(bound) = classifier
                        .type_param_bounds()
                        .get(index)
                        .and_then(|bounds| bounds.first())
                        .copied()
                        .filter(|bound| *bound != Ty::Error)
                    {
                        bindings.insert(
                            formal.clone(),
                            crate::symbol_resolver::ty_subst_keep_unbound(bound, &known),
                        );
                    }
                }
                crate::trace_compiler!(
                    "expected_call",
                    "provider constructor={} expected={expected_classifier:?} generic_params={:?} actuals={actuals:?} upper_only={:?} upper_bounds={:?} bindings={bindings:?}",
                    internal.render(),
                    signature.params,
                    constraints.upper_only,
                    constraints.upper_bounds,
                );
                // Constructor type arguments belong to the classifier. They are fixed before
                // ordinary argument constraints participate (`Foo<String>(42.0)` cannot infer the
                // primary constructor's `T` as `Double`). Kotlin constructors introduce no
                // independent function type parameters, so the classifier's declaration-order
                // formal prefix is the exact binding target. An underscore remains unbound and is
                // completed by the argument/expected-type constraints above.
                for (formal, explicit) in classifier
                    .type_params
                    .iter()
                    .zip(explicit_type_arguments.iter().copied())
                {
                    if !explicit.mentions_error() && !explicit.mentions_pending() {
                        bindings.insert(formal.clone(), explicit);
                    }
                }
                // Expected-result inference runs after argument inference and may replace a class
                // binding (`C<T : Bound>` expected as `I<Unrelated>` through `C<T> : I<T>`).
                // Revalidate the completed classifier bindings here; validating only the earlier
                // argument constraints lets an impossible expected type construct C<Unrelated>.
                let violated_bound = crate::symbol_resolver::generic_binding_bound_violation(
                    signature,
                    &bindings,
                    |actual, bound| self.generic_bound_admits(actual, bound),
                );
                if let Some(failure) = violated_bound {
                    type_argument_bound_failure.get_or_insert_with(|| {
                        (
                            failure,
                            Self::generic_constructor_display(signature, &member),
                            bound_context.as_ref().map(|context| {
                                library_constructor_failure::BoundConstructorDiagnostic::new(
                                    context.outer(),
                                    signature.clone(),
                                )
                            }),
                            Vec::new(),
                        )
                    });
                    continue;
                }
                let result_arguments =
                    crate::symbol_resolver::completed_constructor_result_arguments(
                        internal,
                        classifier.type_params(),
                        classifier.type_param_bounds(),
                        &bindings,
                    );
                member.ret = Ty::obj_args_name(internal, &result_arguments);
                member.params = signature
                    .params
                    .iter()
                    .map(|parameter| {
                        crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &bindings)
                    })
                    .collect();
                inferred = Some(bindings);
            }
            member.call_sig = call_sig;
            candidates.push(CtorDelegationCandidate {
                target: ResolvedCtorDelegationTarget::Super {
                    owner: internal,
                    params: member.params.clone(),
                    declaration_params: declaration_parameter_shapes.clone(),
                    implicit_outer: None,
                    stable_declaration: member.stable_declaration,
                    external_identity: member.external_identity,
                },
                context_count: member.context_count,
                param_names: member.call_sig.param_names.clone(),
                defaults: member.call_sig.param_defaults.to_vec(),
                vararg: member.call_sig.vararg_index,
                supports_default_abi: member.default_realization.is_some(),
                low_priority: member.annotations.contains(&crate::types::type_name(
                    "kotlin/internal/LowPriorityInOverloadResolution",
                )),
                parameter_constraints: declaration_parameter_shapes
                    .iter()
                    .copied()
                    .map(Self::constructor_shape_constraint)
                    .collect(),
                implicit_integer_coercion: member.call_sig.implicit_integer_coercion.to_vec(),
                lambda_receivers: if member.call_sig.lambda_receiver_params.len() == parameter_count
                {
                    member.call_sig.lambda_receiver_params.to_vec()
                } else {
                    Self::semantic_lambda_receiver_flags(&member.params)
                },
            });
            members.push(member);
            member_bindings.push(inferred);
        }

        let (candidate_index, selected) =
            match self.select_source_constructor(scope, &arguments, &candidates) {
                SourceConstructorSelection::Selected {
                    candidate_index,
                    resolved,
                } => (candidate_index, *resolved),
                SourceConstructorSelection::NoMatch { mismatch } => {
                    if let Some(mismatch) = mismatch {
                        return Err(LibraryConstructorFailure::TypeMismatch {
                            source_argument: mismatch.source_argument,
                            expected: mismatch.expected,
                            actual: mismatch.actual,
                            result: members.first().map(|member| member.ret),
                        });
                    }
                    if candidates.is_empty() {
                        if mapping_failures.is_empty() {
                            if let Some(failure) =
                                bound_failures
                                    .into_iter()
                                    .min_by_key(|failure| match failure {
                                        LibraryConstructorFailure::TypeMismatch {
                                            source_argument,
                                            ..
                                        } => *source_argument,
                                        LibraryConstructorFailure::ArgumentContradictions(
                                            contradictions,
                                        ) => contradictions.first_argument(),
                                        LibraryConstructorFailure::Mapping(_)
                                        | LibraryConstructorFailure::TypeArgumentMismatch {
                                            ..
                                        } => usize::MAX,
                                    })
                            {
                                return Err(failure);
                            }
                        }
                        if let Some((
                            (expected, actual),
                            constructor,
                            bound_constructor,
                            postponed_lambdas,
                        )) = type_argument_bound_failure
                        {
                            return Err(LibraryConstructorFailure::TypeArgumentMismatch {
                                expected,
                                actual,
                                classifier: internal,
                                explicit: explicit_type_arguments,
                                constructor,
                                bound_constructor,
                                postponed_lambdas,
                            });
                        }
                        if let Some((failure, (params, call_sig))) =
                            take_unanimous_mapping_error(&mut mapping_failures)
                        {
                            return Err(LibraryConstructorFailure::mapping(
                                failure,
                                params,
                                call_sig,
                                internal.nested_segment_ref().to_string(),
                            ));
                        }
                    }
                    return Ok(LibraryConstructorSelection::NoMatch);
                }
                SourceConstructorSelection::Ambiguous => {
                    return Ok(LibraryConstructorSelection::NoMatch);
                }
            };
        match preselection {
            ConstructorPreselection::Ambiguous => return Ok(LibraryConstructorSelection::NoMatch),
            ConstructorPreselection::Rejected => {
                // The type arguments this declaration infers type the call; its caller reports
                // the rejection.
                if let Some(Some(bindings)) = member_bindings.get(candidate_index) {
                    self.record_library_constructor_type_arguments(
                        call, internal, classifier, bindings,
                    );
                }
                return Ok(LibraryConstructorSelection::Rejected(Ty::Error));
            }
            ConstructorPreselection::Open | ConstructorPreselection::Selected(_) => {}
        }
        let Some(member) = members.get(candidate_index).cloned() else {
            return Ok(LibraryConstructorSelection::NoMatch);
        };
        let annotation = if classifier.is_annotation() {
            let Some(construction) = dependency_annotation_defaults::dependency_construction(
                self.libraries,
                classifier,
                &member,
            ) else {
                return Ok(LibraryConstructorSelection::NoMatch);
            };
            Some(construction)
        } else {
            None
        };
        let bindings = &member_bindings[candidate_index];
        if self.reject_ctor_access(call, internal, classifier, &member, bindings.as_ref()) {
            return Ok(LibraryConstructorSelection::Rejected(Ty::Error));
        }
        let mut slots = vec![None; member.params.len()];
        for (&argument, &slot) in args.iter().zip(&selected.argument_slots) {
            if let Some(target) = slots.get_mut(slot) {
                target.get_or_insert(argument);
            }
        }
        let target = if (!selected.omitted.is_empty()
            || (member.descriptor.is_empty() && member.default_realization.is_some()))
            && member.default_realization.is_some()
        {
            let realization = member
                .default_realization
                .as_deref()
                .expect("checked above");
            ResolvedConstructor::Synthetic {
                owner: internal,
                outer: None,
                ctor: crate::symbol_resolver::SyntheticCtorCall {
                    declaration: member.clone(),
                    descriptor: realization.descriptor.clone(),
                    real_params: realization.real_params.clone(),
                    mask_count: realization.mask_count,
                },
                annotation,
                context_args: selected.context_args.clone(),
                args: args.to_vec(),
            }
        } else if selected.argument_slots == (0..args.len()).collect::<Vec<_>>()
            && selected.omitted.is_empty()
        {
            ResolvedConstructor::Plain {
                owner: internal,
                outer: None,
                member: member.clone(),
                annotation,
                context_args: selected.context_args.clone(),
                args: args.to_vec(),
            }
        } else {
            ResolvedConstructor::PlainSlots {
                owner: internal,
                outer: None,
                member: member.clone(),
                annotation,
                context_args: selected.context_args.clone(),
                slots,
            }
        };
        if !self.expect_selected_constructor_args(scope, call, args, arg_names, &target) {
            return Ok(LibraryConstructorSelection::NoMatch);
        }
        if let Expr::Call { callee, .. } = self.file.expr(call) {
            self.reject_inaccessible_classifier_expression(*callee, internal);
        }
        self.check_selected_constructor_lambdas(scope, args, &selected.argument_types);
        let publication =
            constructor_commitment::ConstructorDeclarationShape::provider(classifier, &member);
        self.commit_constructor(call, target.clone(), publication);
        if let Some(Some(bindings)) = member_bindings.get(candidate_index) {
            self.record_library_constructor_type_arguments(call, internal, classifier, bindings);
        }
        Ok(LibraryConstructorSelection::Selected)
    }
}
