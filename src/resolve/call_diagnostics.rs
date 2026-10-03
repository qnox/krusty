//! Diagnostic ownership for receiver calls whose candidates all rejected the source arguments.

use super::*;

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
            self.expr_inner_lambda(scope, argument, None, params, body, implicit_lambda_label);
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
        receiver: Ty,
        failure: MemberMappingFailure,
    ) -> Option<Ty> {
        let candidates = self
            .stable_receiver_callables(receiver, name)
            .functions()
            .to_vec();
        match self.rejected_call_owner(call_args.call, call_args.args, &candidates) {
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
    pub(super) fn report_extension_receiver_type_mismatch(
        &mut self,
        call: ExprId,
        name: &str,
        candidate: &crate::libraries::FunctionInfo,
        constraints: CallConstraints<'_>,
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
        let receiver_bound_mismatch = candidate.is_extension()
            && signature.receiver.is_some_and(|receiver| {
                signature
                    .formals
                    .iter()
                    .enumerate()
                    .filter(|(_, formal)| {
                        ty_mentions_param(receiver, std::slice::from_ref(*formal))
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

        let display = self
            .source_callable_display(candidate)
            .unwrap_or_else(|| Self::callable_candidate_display(name, candidate));
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
