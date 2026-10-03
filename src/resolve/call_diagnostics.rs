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
}
