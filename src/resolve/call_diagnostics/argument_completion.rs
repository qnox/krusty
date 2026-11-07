//! Complete lambda inputs after a mapped candidate's declaration context is rejected.

use super::super::*;

impl Checker<'_> {
    pub(super) fn complete_rejected_lambda_inputs(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        arguments: &[ExprId],
        candidate: &crate::libraries::FunctionInfo,
        mapping: &[usize],
        bindings: &crate::symbol_resolver::GSigBinds,
    ) {
        let Some(signature) = candidate.generic_sig.as_ref() else {
            return;
        };
        let label = call_implicit_lambda_label(self.file, call);
        for (source, &argument) in arguments.iter().enumerate() {
            let Some(&parameter_index) = mapping.get(source) else {
                continue;
            };
            let Some(parameter) = signature.params.get(parameter_index) else {
                continue;
            };
            let declared = crate::symbol_resolver::ty_subst_keep_unbound(*parameter, bindings);
            let expected = if candidate.call_sig.vararg_index == Some(parameter_index)
                && !self.file.is_spread_arg(argument)
            {
                declared.array_read_elem().unwrap_or(declared)
            } else {
                declared
            };
            let expected = self.declared_function_semantic_type(expected);
            let TyKind::Fun(function) = expected.non_null().kind() else {
                continue;
            };
            let Expr::Lambda { params, .. } = self.file.expr(argument) else {
                continue;
            };
            let layout = lambda_expectation::FunctionLambdaInputLayout::new(
                self.file,
                argument,
                &function.params,
                function.context_count,
                function.has_receiver,
            );
            let inputs = layout.values;
            let named_contexts = self
                .file
                .anon_fun_context_count
                .get(&argument.0)
                .copied()
                .unwrap_or(0) as usize;
            let mut affected = false;
            for index in named_contexts..params.len() {
                if self
                    .file
                    .lambda_param_types
                    .get(&argument.0)
                    .and_then(|types| types.get(index))
                    .is_some_and(Option::is_some)
                    || self
                        .file
                        .lambda_parameter_roles
                        .get(&argument.0)
                        .and_then(|roles| roles.get(index))
                        .is_some_and(|role| *role != LambdaParameterRole::Named)
                {
                    continue;
                }
                let Some(input) = inputs.get(index - named_contexts) else {
                    continue;
                };
                let span = self
                    .file
                    .lambda_param_spans
                    .get(&argument.0)
                    .and_then(|spans| spans.get(index))
                    .copied()
                    .unwrap_or_else(|| self.span(argument));
                for formal in &signature.formals {
                    if bindings.contains_key(formal.as_str())
                        || !ty_mentions_param(*input, std::slice::from_ref(formal))
                    {
                        continue;
                    }
                    self.diags.error(
                        span,
                        format!(
                            "cannot infer type for type parameter '{}'. Specify it explicitly.",
                            crate::types::type_parameter_source_name(formal)
                        ),
                    );
                    affected = true;
                }
            }
            if affected {
                // Recheck only this expression using retained declaration/receiver facts. Other
                // input slots and explicit annotations keep their own types; the failed call's
                // expected result cannot supply a solution for a missing input formal.
                self.postponed_diagnostics.discard(argument);
                let checked = self.check_lambda_with_function_type_labeled(
                    scope, argument, function, false, label,
                );
                self.set(argument, checked);
            }
        }
    }
}
