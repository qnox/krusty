//! Diagnostic ownership for receiver calls whose candidates all rejected the source arguments.

use super::*;

mod argument_completion;

#[derive(Clone, Copy)]
enum ReceiverAdmissionPhase {
    BeforeArguments,
    AfterArguments,
}

fn first_name_use(file: &File, expression: ExprId, name: &str) -> Option<ExprId> {
    match file.expr(expression) {
        Expr::Name(candidate) => (candidate == name).then_some(expression),
        // A nested lambda owns its own implicit parameter and lexical diagnostics.
        Expr::Lambda { .. } => None,
        _ => {
            let found = std::cell::Cell::new(None);
            file.any_child_expr(
                expression,
                &mut |child| {
                    if found.get().is_none() {
                        found.set(first_name_use(file, child, name));
                    }
                    found.get().is_some()
                },
                &mut |statement| {
                    if found.get().is_none() {
                        found.set(first_name_use_in_statement(file, statement, name));
                    }
                    found.get().is_some()
                },
            );
            found.get()
        }
    }
}

fn first_name_use_in_statement(file: &File, statement: StmtId, name: &str) -> Option<ExprId> {
    if matches!(file.stmt(statement), Stmt::LocalFun(_)) {
        return None;
    }
    let found = std::cell::Cell::new(None);
    file.any_child_stmt(statement, &mut |child| {
        if found.get().is_none() {
            found.set(first_name_use(file, child, name));
        }
        found.get().is_some()
    });
    found.get()
}

/// The rejected candidate that owns a receiver call's failure.
pub(super) enum RejectedCallOwner {
    Member,
    Extension(Box<crate::libraries::FunctionInfo>),
    Joined(Vec<crate::libraries::FunctionInfo>),
}

/// The value-parameter types a rejected candidate's arguments map to, in argument order.
fn rejected_candidate_argument_shape(
    candidate: &crate::libraries::FunctionInfo,
    argument_count: usize,
    argument_names: Option<&[Option<String>]>,
    trailing_lambda: bool,
) -> Vec<Ty> {
    let context_count = candidate
        .context_count
        .min(candidate.semantic_params().len());
    let signature = candidate.call_sig.suffix(context_count);
    let parameters = candidate.value_params();
    (0..argument_count)
        .filter_map(|argument| {
            let parameter = match argument_names
                .and_then(|names| names.get(argument))
                .and_then(Option::as_deref)
            {
                Some(name) => signature.param_names.iter().position(|param| param == name),
                None if trailing_lambda && argument + 1 == argument_count => {
                    parameters.len().checked_sub(1)
                }
                None if argument < parameters.len() => Some(argument),
                None => signature.vararg_index,
            };
            parameter.and_then(|parameter| parameters.get(parameter).copied())
        })
        .collect()
}

impl Checker<'_> {
    /// When overload selection rejects a family by type but exactly one declaration has a valid
    /// source-argument mapping, report that declaration's mapped argument errors. This diagnostic
    /// step is origin-neutral: top-level and extension candidates carry the same semantic shape and
    /// must not fall back to a generic inapplicable message merely because one came from a package
    /// receiver.
    pub(super) fn report_single_mapped_candidate_type_errors(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        name: &str,
        args: &[ExprId],
        arg_tys: &[Ty],
        constraints: CallConstraints<'_>,
        candidates: &[crate::libraries::FunctionInfo],
    ) -> Option<Ty> {
        let type_args = constraints.type_args;
        let argument_names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        let checkpoint = self.diags.diags.len();
        let mut mapped = candidates
            .iter()
            .filter_map(|candidate| {
                let signature = candidate.semantic_signature();
                let type_arity_matches =
                    type_args.is_empty() || signature.formals.len() == type_args.len();
                let mut bindings = constraints.bindings(&self.fed_source(), &signature);
                let initial_params = signature
                    .params
                    .iter()
                    .map(|&parameter| {
                        crate::types::ty_subst_formals(parameter, &signature.formals, &bindings)
                    })
                    .collect::<Vec<_>>();
                let (mut shape, missing_context) = match self.contextual_call_shape_result(
                    scope,
                    &initial_params,
                    &candidate.call_sig,
                    candidate.context_count,
                    argument_names,
                ) {
                    Ok(shape) => (shape, None),
                    Err(missing) => {
                        let context_count = candidate.context_count.min(initial_params.len());
                        let explicitly_named = |parameter: usize| {
                            self.file.explicit_context_arguments
                                && candidate.call_sig.param_names.get(parameter).is_some_and(
                                    |parameter| {
                                        argument_names.is_some_and(|names| {
                                            names
                                                .iter()
                                                .flatten()
                                                .any(|argument| argument == parameter)
                                        })
                                    },
                                )
                        };
                        let parameter_indices = (context_count..initial_params.len())
                            .chain(
                                (0..context_count).filter(|&parameter| explicitly_named(parameter)),
                            )
                            .collect::<Vec<_>>();
                        let selected_context_indices = (0..context_count)
                            .filter(|&parameter| !explicitly_named(parameter))
                            .collect::<Vec<_>>();
                        let mut context_sources = vec![None; context_count];
                        let mut context_actual_types = vec![None; context_count];
                        for parameter in selected_context_indices {
                            let Ok(selected) = self.select_context_arguments_with_types(
                                scope,
                                std::slice::from_ref(&initial_params[parameter]),
                            ) else {
                                continue;
                            };
                            let Some((source, actual)) = selected.into_iter().next() else {
                                continue;
                            };
                            context_sources[parameter] = Some(source);
                            context_actual_types[parameter] = Some(actual);
                        }
                        let shape = ContextualCallShape {
                            params: parameter_indices
                                .iter()
                                .map(|&parameter| initial_params[parameter])
                                .collect(),
                            call_sig: candidate.call_sig.select_parameters(&parameter_indices),
                            parameter_indices,
                            context_sources,
                            context_actual_types,
                        };
                        // Context lookup consumes the applied shape, but kotlinc names an
                        // unavailable parameter with its declaration type (`x: T`, not `x: String`).
                        let diagnostic_missing = MissingContextParameter {
                            index: missing.index,
                            ty: signature.params[missing.index],
                        };
                        (shape, Some(diagnostic_missing))
                    }
                };
                for (parameter, actual) in shape.context_actual_types.iter().enumerate() {
                    let Some(actual) = actual else { continue };
                    let declared = *signature.params.get(parameter)?;
                    unify_member_extension_slot(declared, *actual, &[], &mut bindings);
                }
                let params = signature
                    .params
                    .iter()
                    .map(|&parameter| {
                        crate::types::ty_subst_formals(parameter, &signature.formals, &bindings)
                    })
                    .collect::<Vec<_>>();
                shape.params = shape
                    .parameter_indices
                    .iter()
                    .map(|&parameter| params[parameter])
                    .collect();
                let parameters = call_argument_parameter_indices(
                    args.len(),
                    shape.params.len(),
                    argument_names,
                    trailing_lambda,
                    &shape.call_sig,
                )?;
                let omitted_defaults = map_call_sig_args_with_trailing(
                    args,
                    argument_names,
                    shape.params.len(),
                    &shape.call_sig,
                    trailing_lambda,
                )
                .ok()?
                .iter()
                .filter(|argument| argument.is_none())
                .count();
                let specificity = parameters
                    .iter()
                    .enumerate()
                    .map(|(source, &parameter)| {
                        let declared = shape.params[parameter];
                        if shape.call_sig.vararg_index == Some(parameter)
                            && !self.file.is_spread_arg(args[source])
                        {
                            declared.array_read_elem().unwrap_or(declared)
                        } else {
                            declared
                        }
                    })
                    .collect::<Vec<_>>();
                let generic = candidate
                    .generic_sig
                    .as_ref()
                    .is_some_and(|signature| !signature.formals.is_empty());
                Some((
                    candidate,
                    shape,
                    parameters,
                    specificity,
                    omitted_defaults,
                    generic,
                    missing_context,
                    type_arity_matches,
                ))
            })
            .collect::<Vec<_>>();
        // Scope-tower priority chooses the nearest APPLICABLE level. A nearer declaration whose
        // source arguments do not even map does not hide a mapped declaration on an outer level
        // (`with(outer) { with(inner) { receiver.extension(null) } }`). Selection already follows
        // that rule; rejected-call recovery must project the same tower before deciding which one
        // candidate owns the argument-type diagnostic.
        if let Some(nearest_scope_rank) = mapped
            .iter()
            .map(|(candidate, ..)| (candidate.scope_rung.selection_rank(), candidate.scope_rank))
            .min()
        {
            mapped.retain(|(candidate, ..)| {
                (candidate.scope_rung.selection_rank(), candidate.scope_rank) == nearest_scope_rank
            });
        }
        let any_type_arity_matches = mapped
            .iter()
            .any(|(_, _, _, _, _, _, _, type_arity_matches)| *type_arity_matches);
        if any_type_arity_matches {
            mapped.retain(|(_, _, _, _, _, _, _, type_arity_matches)| *type_arity_matches);
        }
        if mapped.len() > 1 {
            let dominated = mapped
                .iter()
                .enumerate()
                .filter_map(|(index, (_, _, _, specificity, omitted, ..))| {
                    mapped
                        .iter()
                        .any(|(_, _, _, other_specificity, other_omitted, ..)| {
                            other_specificity == specificity && other_omitted < omitted
                        })
                        .then_some(index)
                })
                .collect::<crate::fx_hash::HashSet<_>>();
            mapped = mapped
                .into_iter()
                .enumerate()
                .filter_map(|(index, candidate)| (!dominated.contains(&index)).then_some(candidate))
                .collect();
        }
        crate::symbol_resolver::retain_most_specific_declarations(
            &self.fed_source(),
            Some(constraints.receiver),
            &mut mapped,
            |(candidate, _, _, specificity, _, generic, _, _)| {
                (
                    candidate
                        .is_extension()
                        .then(|| candidate.semantic_receiver())
                        .flatten(),
                    specificity.as_slice(),
                    *generic,
                    Some(candidate.callable.owner),
                )
            },
        );
        if !any_type_arity_matches {
            if let [(candidate, ..)] = mapped.as_slice() {
                if let Some(formal) = candidate
                    .generic_sig
                    .as_ref()
                    .and_then(|signature| signature.formals.get(type_args.len()))
                {
                    self.diags.error(
                        self.call_callee_name_span(call),
                        format!(
                            "cannot infer type for type parameter '{}'. Specify it explicitly.",
                            crate::types::type_parameter_source_name(formal)
                        ),
                    );
                }
            }
        }
        let reported_type_arity = if any_type_arity_matches {
            false
        } else {
            let candidates = mapped
                .iter()
                .map(|(candidate, ..)| (*candidate).clone())
                .collect::<Vec<_>>();
            self.report_callable_type_argument_arity(call, name, type_args.len(), &candidates)
        };
        let [(candidate, shape, parameters, _, _, _, missing_context, _)] = mapped.as_slice()
        else {
            return reported_type_arity.then_some(Ty::Error);
        };
        if let Some(missing) = missing_context {
            self.report_missing_context_parameter(call, *missing, &candidate.call_sig.param_names);
        }
        if self.report_dnn_null_inference(
            call,
            candidate,
            args,
            argument_names,
            trailing_lambda,
            type_args.len(),
        ) {
            self.diags.sort_source_order_from(checkpoint);
            return Some(Ty::Error);
        }
        if candidate.constructor_alias.is_some() {
            if let Some(result) = self.report_extension_receiver_type_mismatch(
                call,
                name,
                candidate,
                constraints,
                ReceiverAdmissionPhase::BeforeArguments,
            ) {
                if let Some(signature) = candidate.generic_sig.as_ref() {
                    let bindings = CallConstraints {
                        expected: None,
                        ..constraints
                    }
                    .bindings(&self.fed_source(), signature);
                    let mapping = parameters
                        .iter()
                        .map(|&parameter| shape.parameter_indices[parameter])
                        .collect::<Vec<_>>();
                    self.complete_rejected_lambda_inputs(
                        scope, call, args, candidate, &mapping, &bindings,
                    );
                }
                self.diags.sort_source_order_from(checkpoint);
                return Some(result);
            }
        }
        if let Some(signature) = candidate.generic_sig.as_ref() {
            let bindings = crate::symbol_resolver::infer_generic_call_bindings(
                signature,
                parameters.iter().enumerate().map(|(source, &parameter)| {
                    let argument = args[source];
                    let declaration_parameter = shape.parameter_indices[parameter];
                    let actual = self
                        .callable_reference_types
                        .get(&argument)
                        .copied()
                        .unwrap_or(arg_tys[source]);
                    (declaration_parameter, actual, false)
                }),
                candidate.call_sig.vararg_index,
            );
            for (formal_index, formal) in signature.formals.iter().enumerate() {
                let inferred_from_callable_reference =
                    parameters.iter().enumerate().any(|(source, &parameter)| {
                        let declaration_parameter = shape.parameter_indices[parameter];
                        matches!(self.file.expr(args[source]), Expr::CallableRef { .. })
                            && signature.params.get(declaration_parameter).is_some_and(
                                |parameter| {
                                    ty_mentions_param(*parameter, std::slice::from_ref(formal))
                                },
                            )
                    });
                if !inferred_from_callable_reference {
                    continue;
                }
                let satisfies_bounds = bindings.get(formal).is_some_and(|&actual| {
                    signature
                        .formal_bounds
                        .get(formal_index)
                        .into_iter()
                        .flatten()
                        .all(|&bound| self.receiver_is_assignable(actual, bound))
                });
                if !satisfies_bounds {
                    self.diags.error(
                        self.span(call),
                        format!(
                            "cannot infer type for type parameter '{}'. Specify it explicitly.",
                            crate::types::type_parameter_source_name(formal)
                        ),
                    );
                    return Some(Ty::Error);
                }
            }
        }
        let formal_owners = candidate
            .constructor_alias
            .as_deref()
            .map(|alias| self.publish_constructor_alias_rigid_formals(alias))
            .unwrap_or_default();
        for (source, &parameter) in parameters.iter().enumerate() {
            let Some((&declared, &actual)) = shape.params.get(parameter).zip(arg_tys.get(source))
            else {
                continue;
            };
            let expected = if shape.call_sig.vararg_index == Some(parameter)
                && !self.file.is_spread_arg(args[source])
            {
                declared.array_read_elem().unwrap_or(declared)
            } else {
                declared
            };
            if !shape.call_sig.parameter_admits(parameter, expected, actual) {
                self.report_assignability_error(
                    expected,
                    actual,
                    self.span(args[source]),
                    "argument",
                );
            } else {
                self.expect_call_arg(scope, expected, args[source], actual);
            }
        }
        self.restore_type_parameter_owners(formal_owners);
        if self.diags.diags.len() > checkpoint {
            let signature = candidate.semantic_signature();
            let bindings = constraints.bindings(&self.fed_source(), &signature);
            self.report_explicit_type_argument_bound_errors(
                call, name, candidate, &signature, &bindings,
            );
            self.diags.sort_source_order_from(checkpoint);
            return Some(Ty::Error);
        }

        if let Some(result) = self.report_extension_receiver_type_mismatch(
            call,
            name,
            candidate,
            constraints,
            ReceiverAdmissionPhase::AfterArguments,
        ) {
            return Some(result);
        }

        let signature = candidate.semantic_signature();
        let bindings = constraints.bindings(&self.fed_source(), &signature);
        let bound_checkpoint = self.diags.diags.len();
        self.report_explicit_type_argument_bound_errors(
            call, name, candidate, &signature, &bindings,
        );
        if self.diags.diags.len() > bound_checkpoint {
            let mut display = self
                .source_callable_display(candidate)
                .unwrap_or_else(|| Self::callable_candidate_display(name, candidate));
            if candidate.classifier_static {
                display.insert_str(0, "static ");
            }
            self.diags.error(
                self.call_callee_name_span(call),
                format!("inapplicable candidate(s): {display}"),
            );
            self.diags.sort_source_order_from(checkpoint);
            return Some(Ty::Error);
        }
        None
    }

    /// Report the exact unavailable context parameter retained on a selected callable. Candidate
    /// selection already chose the declaration; diagnostics consume that identity and its applied
    /// semantic signature instead of falling back to a family-level context message.
    pub(super) fn report_selected_callable_missing_context(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        selected: &SelectedCallable,
    ) {
        let parameters = selected.applied_params();
        let argument_names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
        let missing = self
            .contextual_call_shape_result(
                scope,
                &parameters,
                &selected.call_sig,
                selected.context_count,
                argument_names,
            )
            .err();
        if let Some(missing) = missing {
            self.report_missing_context_parameter(call, missing, &selected.call_sig.param_names);
        } else {
            self.diags.error(
                self.call_callee_name_span(call),
                "no implicit value is available for the context parameters".to_string(),
            );
        }
    }

    /// Report a rejected sole candidate's explicitly written type arguments against its bounds,
    /// adding kotlinc's candidate diagnostic when the call also has value arguments.
    pub(super) fn report_rejected_explicit_type_argument_bounds(
        &mut self,
        call: ExprId,
        candidate: &crate::libraries::FunctionInfo,
        has_value_arguments: bool,
        explicit: &[Ty],
    ) -> bool {
        let Some(signature) = candidate.generic_sig.as_ref() else {
            return false;
        };
        let checkpoint = self.diags.diags.len();
        let bindings = crate::symbol_resolver::seeded_gsig_binds(signature, explicit);
        self.report_explicit_type_argument_bound_errors(
            call,
            &candidate.callable.name,
            candidate,
            signature,
            &bindings,
        );
        if self.diags.diags.len() == checkpoint {
            return false;
        }
        if has_value_arguments {
            let display = self.source_callable_display(candidate).unwrap_or_else(|| {
                Self::callable_candidate_display(&candidate.callable.name, candidate)
            });
            self.diags.error(
                self.call_callee_name_span(call),
                format!("inapplicable candidate(s): {display}"),
            );
        }
        true
    }

    /// Retain upper-bound diagnostics when a rejected call still has one shape-matching candidate.
    /// Recovery uses the declaration's result so enclosing expressions remain typed, but an
    /// explicitly written invalid type argument remains an error owned by its source occurrence.
    pub(super) fn report_explicit_type_argument_bound_errors(
        &mut self,
        call: ExprId,
        name: &str,
        candidate: &crate::libraries::FunctionInfo,
        signature: &crate::libraries::GenericSig,
        bindings: &crate::symbol_resolver::GSigBinds,
    ) {
        let Some(type_arguments) = self
            .file
            .call_type_args
            .get(&call.0)
            .map(|syntax| syntax.arguments.clone())
        else {
            return;
        };
        let declaration = Self::generic_callable_header(name, candidate);
        for (formal_index, type_argument) in type_arguments.iter().enumerate() {
            if type_argument.is_inference_placeholder() {
                continue;
            }
            let Some(formal) = signature.formals.get(formal_index) else {
                continue;
            };
            let Some(&actual) = bindings.get(formal) else {
                continue;
            };
            for &bound in signature
                .formal_bounds
                .get(formal_index)
                .into_iter()
                .flatten()
            {
                let expected = crate::symbol_resolver::ty_subst_keep_unbound(bound, bindings);
                if self.generic_bound_admits(actual, expected) {
                    continue;
                }
                // A flexible Java bound exposes the written argument through that same platform
                // view in diagnostics (`T : Number!`, `String!`). This is declaration metadata,
                // not an origin branch: a non-flexible Kotlin bound keeps the written type exact.
                let diagnostic_actual = if expected.is_platform_nullable() {
                    expected.rewrap_nullability(actual.non_null())
                } else {
                    actual
                };
                let context = [expected, diagnostic_actual];
                let expected = self.diagnostic_type_name(expected, &context);
                let actual = self.diagnostic_type_name(diagnostic_actual, &context);
                let formal = crate::types::type_parameter_source_name(formal);
                self.diags.error(
                    type_argument.span,
                    format!(
                        "type argument is not within its bounds: type parameter '{formal} (of {declaration})' must be subtype of '{expected}', but actual: '{actual}'."
                    ),
                );
            }
        }
    }

    /// Report a generic call whose contextual result constraints disagree on a formal that also
    /// shapes a lambda argument. The call itself owns the inference failure, but each affected
    /// lambda remains a separately diagnosed source expression and its body must still be checked
    /// without the unavailable contextual input. This preserves the complete source-ordered
    /// diagnostic set instead of returning after the callee error and hiding body failures.
    pub(super) fn report_contextual_result_inference_failure(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
        argument_parameters: Option<&[usize]>,
        signature: &crate::libraries::GenericSig,
        formal: &String,
        implicit_lambda_label: Option<&str>,
    ) {
        let message = format!(
            "cannot infer type for type parameter '{}'. Specify it explicitly.",
            crate::types::type_parameter_source_name(formal)
        );
        self.diags
            .error(self.call_callee_name_span(call), message.clone());

        for (source, &argument) in args.iter().enumerate() {
            let Some(parameter) = argument_parameters
                .and_then(|parameters| parameters.get(source))
                .and_then(|&parameter| signature.params.get(parameter))
            else {
                continue;
            };
            if !ty_mentions_param(*parameter, std::slice::from_ref(formal)) {
                continue;
            }
            let Expr::Lambda { params, body } = self.file.expr(argument).clone() else {
                continue;
            };
            self.diags.error(self.span(argument), message.clone());
            self.omit_unshaped_any_receiver_type += 1;
            self.expr_inner_lambda(scope, argument, None, params, body, implicit_lambda_label);
            self.omit_unshaped_any_receiver_type -= 1;
        }
    }

    /// Report the diagnostics owned by a lambda for which no callable supplied an expected
    /// function shape. This runs only after the scope tower is exhausted: probes remain silent,
    /// while the final rejected source form diagnoses every untyped written parameter and leaves
    /// an implicit `it` unresolved, as the language does without a contextual function type.
    pub(super) fn report_unshaped_lambda_diagnostics(
        &mut self,
        scope: &CheckerScope<'_>,
        arguments: &[ExprId],
    ) {
        for &argument in arguments {
            let Expr::Lambda { params, body } = self.file.expr(argument) else {
                continue;
            };
            if params.is_empty() {
                if !self.file.lambda_explicit_arrows.contains(&argument.0)
                    && self.untyped_lambda_binds_implicit_it(scope, *body)
                {
                    if let Some(reference) = first_name_use(self.file, *body, "it") {
                        self.diags.error(
                            self.span(reference),
                            "unresolved reference 'it'.".to_string(),
                        );
                    }
                }
                continue;
            }

            let declared = self.file.lambda_param_types.get(&argument.0);
            let roles = self.file.lambda_parameter_roles.get(&argument.0);
            let spans = self.file.lambda_param_spans.get(&argument.0);
            for (index, parameter) in params.iter().enumerate() {
                let explicitly_typed = declared
                    .and_then(|types| types.get(index))
                    .is_some_and(Option::is_some);
                let named = roles
                    .and_then(|roles| roles.get(index))
                    .is_none_or(|role| *role == LambdaParameterRole::Named);
                if explicitly_typed || !named {
                    continue;
                }
                let span = spans
                    .and_then(|spans| spans.get(index))
                    .copied()
                    .unwrap_or_else(|| self.span(argument));
                self.diags.error(
                    span,
                    format!(
                        "cannot infer type for value parameter '{parameter}'. Specify it explicitly."
                    ),
                );
            }
        }
    }

    /// Report a receiver call whose member and extension rungs all rejected the source arguments.
    pub(super) fn report_owned_member_failure(
        &mut self,
        call_args: CallArgs<'_>,
        name: &str,
        candidates: &[crate::libraries::FunctionInfo],
        failure: MemberMappingFailure,
    ) -> Option<Ty> {
        match self.rejected_call_owner(call_args.call, call_args.args, candidates) {
            RejectedCallOwner::Extension(_) => return None,
            RejectedCallOwner::Member => self.report_retained_member_mapping_failure(
                call_args.call,
                name,
                call_args.args,
                failure,
            ),
            RejectedCallOwner::Joined(contenders) => {
                self.report_joined_rejection(call_args.call, name, &contenders)
            }
        }
        Some(Ty::Error)
    }

    /// Choose the rejected candidate that owns the diagnostic by the target version's policy.
    pub(super) fn rejected_call_owner(
        &self,
        call: ExprId,
        args: &[ExprId],
        candidates: &[crate::libraries::FunctionInfo],
    ) -> RejectedCallOwner {
        let contested = crate::diagnostic_wording::inapplicable_member_joins_extensions()
            && candidates.iter().any(|candidate| !candidate.is_extension())
            && candidates.iter().any(|candidate| candidate.is_extension());
        if !contested {
            return RejectedCallOwner::Member;
        }
        let argument_names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        let mut contenders = candidates
            .iter()
            .map(|candidate| {
                let shape = rejected_candidate_argument_shape(
                    candidate,
                    args.len(),
                    argument_names,
                    trailing_lambda,
                );
                let generic = candidate
                    .generic_sig
                    .as_ref()
                    .is_some_and(|signature| !signature.formals.is_empty());
                (candidate, shape, generic)
            })
            .collect::<Vec<_>>();
        crate::symbol_resolver::retain_most_specific_declarations(
            &self.fed_source(),
            None,
            &mut contenders,
            |(candidate, shape, generic)| {
                let receiver = candidate
                    .is_extension()
                    .then(|| candidate.semantic_receiver())
                    .flatten();
                (receiver, shape.as_slice(), *generic, None)
            },
        );
        crate::trace_compiler!(
            "resolve",
            "rejected call owner call={call:?} candidates={} contenders={:?}",
            candidates.len(),
            contenders
                .iter()
                .map(|(candidate, shape, generic)| (candidate.is_extension(), shape, generic))
                .collect::<Vec<_>>(),
        );
        match contenders.as_slice() {
            [(only, _, _)] if only.is_extension() => {
                RejectedCallOwner::Extension(Box::new((*only).clone()))
            }
            [_] => RejectedCallOwner::Member,
            _ => RejectedCallOwner::Joined(
                contenders
                    .into_iter()
                    .map(|(candidate, _, _)| candidate.clone())
                    .collect(),
            ),
        }
    }

    /// Report a receiver call none of whose same-named `candidates` accepts the arguments: tied
    /// member and extension contenders, a mapping failure every candidate shares, the type
    /// mismatches of the one candidate the arguments map onto, or else NONE_APPLICABLE.
    pub(super) fn report_inapplicable_receiver_candidates(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        name: &str,
        constraints: CallConstraints<'_>,
        candidates: &[crate::libraries::FunctionInfo],
    ) {
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let reported = self.report_member_and_extensions_inapplicable(call, name, args, candidates)
            || self.report_inapplicable_member_mapping_error(call, name, args, candidates)
            || self
                .report_single_mapped_candidate_type_errors(
                    scope,
                    call,
                    name,
                    args,
                    arg_tys,
                    constraints,
                    candidates,
                )
                .is_some();
        if !reported {
            self.diags.error(
                self.call_callee_name_span(call),
                self.inapplicable_member_candidates_message(name, candidates),
            );
        }
    }

    /// Report NONE_APPLICABLE over tied rejected candidates.
    pub(super) fn report_member_and_extensions_inapplicable(
        &mut self,
        call: ExprId,
        name: &str,
        args: &[ExprId],
        candidates: &[crate::libraries::FunctionInfo],
    ) -> bool {
        match self.rejected_call_owner(call, args, candidates) {
            RejectedCallOwner::Joined(contenders) => {
                self.report_joined_rejection(call, name, &contenders);
                true
            }
            RejectedCallOwner::Member | RejectedCallOwner::Extension(_) => false,
        }
    }

    pub(super) fn report_joined_rejection(
        &mut self,
        call: ExprId,
        name: &str,
        contenders: &[crate::libraries::FunctionInfo],
    ) {
        self.diags.error(
            self.call_callee_name_span(call),
            self.inapplicable_member_candidates_message(name, contenders),
        );
    }

    pub(super) fn report_retained_member_mapping_failure(
        &mut self,
        call: ExprId,
        name: &str,
        args: &[ExprId],
        retained: MemberMappingFailure,
    ) {
        let candidate = retained.candidate;
        self.report_callable_arg_mapping_error(
            call,
            args,
            DiagnosticFunction {
                name,
                params: &candidate.semantic_params(),
                param_names: &candidate.call_sig.param_names,
                param_defaults: &candidate.call_sig.param_defaults,
                required: candidate.call_sig.required,
                vararg: candidate.call_sig.vararg,
                context_count: candidate.context_count,
                ret: candidate.callable.ret,
                // Retained mapping failures can name an inherited declaration whose SourceMember
                // ordinal belongs to a different bounded parse. The selected FunctionInfo already
                // carries the complete stable generic/default/context shape; render that record
                // directly instead of indexing the current transient AST.
                source_display: Some(Self::generic_callable_display(name, &candidate)),
            },
            retained.failure,
        );
    }

    pub(super) fn same_argument_mapping_shape(
        first: &crate::libraries::FunctionInfo,
        candidate: &crate::libraries::FunctionInfo,
    ) -> bool {
        candidate.semantic_receiver() == first.semantic_receiver()
            && candidate.semantic_params() == first.semantic_params()
            && candidate.context_count == first.context_count
            && candidate.call_sig.param_names == first.call_sig.param_names
            && candidate.call_sig.param_defaults == first.call_sig.param_defaults
            && candidate.call_sig.required == first.call_sig.required
            && candidate.call_sig.vararg == first.call_sig.vararg
            && candidate.call_sig.vararg_index == first.call_sig.vararg_index
    }

    /// Report a mapped extension rejected only by its declaration receiver or receiver bounds.
    fn report_extension_receiver_type_mismatch(
        &mut self,
        call: ExprId,
        name: &str,
        candidate: &crate::libraries::FunctionInfo,
        constraints: CallConstraints<'_>,
        phase: ReceiverAdmissionPhase,
    ) -> Option<Ty> {
        // A single extension candidate whose arguments map cleanly can still be rejected by its
        // declaration receiver or by a bound on a type variable contributed by that receiver. Keep
        // this declaration-aware: the same normalized candidate shape covers source, module and
        // dependency callables, and no spelling or provider-origin branch participates.
        let signature = candidate.semantic_signature();
        let bindings = constraints.bindings(&self.fed_source(), &signature);
        let declared_receiver = signature
            .receiver
            .or_else(|| candidate.semantic_receiver())
            .map(|receiver| crate::symbol_resolver::ty_subst_keep_unbound(receiver, &bindings));
        let receiver_type_mismatch = candidate.is_extension()
            && declared_receiver.is_some_and(|declared| {
                !self.receiver_is_assignable(constraints.receiver, declared)
            });
        let receiver_bound_mismatch = matches!(phase, ReceiverAdmissionPhase::AfterArguments)
            && candidate.is_extension()
            && signature.receiver.is_some_and(|receiver| {
                signature
                    .formals
                    .iter()
                    .enumerate()
                    .filter(|(_, formal)| {
                        candidate.constructor_alias.is_some()
                            || ty_mentions_param(receiver, std::slice::from_ref(*formal))
                    })
                    .any(|(index, formal)| {
                        let Some(&actual) = bindings.get(formal) else {
                            return false;
                        };
                        signature
                            .formal_bounds
                            .get(index)
                            .into_iter()
                            .flatten()
                            .copied()
                            .map(|bound| {
                                crate::symbol_resolver::ty_subst_keep_unbound(bound, &bindings)
                            })
                            .any(|bound| !self.generic_bound_admits(actual, bound))
                    })
            });
        if !receiver_type_mismatch && !receiver_bound_mismatch {
            return None;
        }

        if receiver_type_mismatch
            && candidate.constructor_alias.is_some()
            && constraints.type_args.is_empty()
        {
            // An inadmissible alias receiver does not open argument/result inference for its
            // constructor. Preserve formals already bound by the receiver itself, but report each
            // remaining alias formal even when an argument or expected result would have solved it.
            let receiver_bindings = CallConstraints {
                expected: None,
                ..constraints
            }
            .bindings(&self.fed_source(), &signature);
            for formal in &signature.formals {
                if !receiver_bindings.contains_key(formal) {
                    self.diags.error(
                        self.call_callee_name_span(call),
                        format!(
                            "cannot infer type for type parameter '{}'. Specify it explicitly.",
                            crate::types::type_parameter_source_name(formal)
                        ),
                    );
                }
            }
        }

        let display = self.source_callable_display(candidate).unwrap_or_else(|| {
            Self::callable_candidate_display_with_alias_constructor(
                name,
                candidate,
                |signature, member| {
                    Self::generic_constructor_display_with_type(signature, member, &|ty| {
                        self.declared_function_semantic_type(ty).source_name()
                    })
                },
            )
        });
        self.diags.error(
            self.call_callee_name_span(call),
            format!("candidate '{display}' is inapplicable because of a receiver type mismatch."),
        );

        // Although the candidate is inapplicable, kotlinc retains its inferred result for the
        // enclosing expression's own type check. Returning that provisional type lets the existing
        // initializer/return/assignment boundary emit its context-specific mismatch without
        // committing a selected call or inventing that diagnostic here.
        // A projection is a constraint on a classifier argument, not a value type. Materialize the
        // provisional result through the same position-aware specialization as a selected call:
        // `List<*>` constraining `T` makes `fun <T> ...: List<T>` read as `List<Any?>`, not as the
        // non-denotable recovery type `List<*>`.
        let inferred = crate::symbol_resolver::specialize_signature_output_type(
            &self.fed_source(),
            signature.ret,
            &bindings,
        );
        Some(
            candidate
                .ret
                .apply(signature.apply_return_policy(self.libraries, inferred)),
        )
    }
}
