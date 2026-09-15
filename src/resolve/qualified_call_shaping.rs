//! Reshaping a selected call's lambda arguments, and retiring the probe that preceded it.
//!
//! A call's arguments must be typed before a candidate can be chosen, so a lambda is first judged
//! with no expected shape: a receiver lambda has no receiver to resolve against and reports
//! unresolved references for members that are perfectly real. Those diagnostics are held aside, the
//! probe's account of them is dropped, and the shared selected-argument commit rechecks the lambdas
//! against the parameters the selected callable declares.
//!
//! The qualified top-level fallback's named-argument mapping lives here as well: it is part of the
//! same package-qualified call path, while the shared selected-call path remains the sole owner of
//! final argument mapping and checking.

use super::*;

type MappedNamedArgs = (Vec<ExprId>, Vec<Ty>, Vec<Option<ExprId>>);

impl Checker<'_> {
    /// Type a call's arguments before any candidate is known, holding the result aside.
    ///
    /// This is a PROBE: a lambda is judged with no expected shape, so `flag = true` in a receiver
    /// lambda has no receiver to resolve against and reports an unresolved reference for a member
    /// that is perfectly real. The diagnostics are captured the way the bare-name path captures its
    /// own — a call that never resolves still commits them — and the returned mark is where they
    /// were taken from, so anything authoritative can be returned there in source order.
    pub(super) fn probe_argument_types(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
    ) -> (Vec<Ty>, usize) {
        let probe_mark = self.diags.diags.len();
        let arg_tys = self.arg_tys(scope, args);
        self.postponed_diagnostics
            .capture_since(call, self.diags, probe_mark);
        (arg_tys, probe_mark)
    }

    /// Retire exactly the lambda diagnostics from a successful argument probe.
    ///
    /// [`Checker::finish_top_level_call`] commits every source argument through the shared selected
    /// argument mapper immediately after this operation. That owner applies named/default/vararg and
    /// context mapping together with the selected generic substitution; duplicating any of that here
    /// makes qualified spelling a second overload/argument path.
    pub(super) fn retire_selected_lambda_probe(
        &mut self,
        call: ExprId,
        args: &[ExprId],
        probe_mark: usize,
    ) {
        // The probe's LAMBDA diagnostics described a body with no shape; selected-argument commit
        // supersedes them. Everything else in the batch — an ordinary argument that does not resolve,
        // for instance — is authoritative and must still reach the user, so only the lambda spans are
        // dropped and the rest returns to the sink in source order. Dropping the whole batch lets a
        // call select through `Ty::Error` and report nothing at all.
        let lambda_spans: Vec<crate::diag::Span> = args
            .iter()
            .filter(|&&argument| matches!(self.file.expr(argument), Expr::Lambda { .. }))
            .map(|&argument| self.span(argument))
            .collect();
        self.postponed_diagnostics
            .discard_within(call, self.diags, probe_mark, &lambda_spans);
    }

    /// Map named arguments for the legacy symbol fallback of a package-qualified top-level call.
    ///
    /// The primary candidate path has already run before this fallback and owns final selected-call
    /// commitment. This operation only preserves source argument order for the older symbol result.
    pub(super) fn map_named_qualified_top_level_args(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        name: &str,
        args: &[ExprId],
        names: &[Option<String>],
        trailing_lambda: bool,
        candidates: Vec<crate::libraries::FunctionInfo>,
    ) -> Result<Option<MappedNamedArgs>, ()> {
        let diagnostic_candidates = candidates.clone();
        let overloads = crate::libraries::FunctionSet {
            overloads: candidates,
        }
        .into_top_level_with_param_names()
        .collect::<Vec<_>>();
        let mut mapped = Vec::new();
        let mut failures = Vec::new();
        for candidate in overloads {
            // CONTEXT parameters are not value arguments: they are supplied by the enclosing scope,
            // so labels and arity are mapped against the context-free signature.
            let context_count = candidate.context_count.min(candidate.callable.params.len());
            let value_signature = candidate.call_sig.suffix(context_count);
            let value_params = &candidate.callable.params[context_count..];
            match map_call_sig_args_with_trailing(
                args,
                Some(names),
                value_params.len(),
                &value_signature,
                trailing_lambda,
            ) {
                Ok(slots) => mapped.push((
                    self.call_slot_score_vararg(
                        value_params,
                        &slots,
                        candidate.call_sig.vararg_index,
                    ),
                    slots,
                    candidate,
                )),
                Err(error) => failures.push((error, candidate)),
            }
        }
        if !mapped.is_empty() && mapped.iter().all(|(score, _, _)| score.is_none()) {
            if mapped.len() == 1 {
                let (_, slots, candidate) = mapped.pop().unwrap();
                for (parameter, argument) in candidate.callable.params.iter().zip(&slots) {
                    if let Some(argument) = argument {
                        self.expect_assignable(
                            *parameter,
                            self.expr_types[argument.0 as usize],
                            self.span(*argument),
                            "argument",
                        );
                    }
                }
            } else if !self.call_already_has_argument_diagnostic(call, args) {
                self.diags.error(
                    self.call_callee_name_span(call),
                    INAPPLICABLE_OVERLOAD_PREFIX.to_string(),
                );
            }
            return Err(());
        }
        mapped.retain(|(score, _, _)| score.is_some());
        mapped.sort_by_key(|(score, _, _)| std::cmp::Reverse(*score));
        if let Some((_, slots, _)) = mapped.into_iter().next() {
            let selected_args = slots.iter().copied().flatten().collect::<Vec<_>>();
            let selected_types = selected_args
                .iter()
                .map(|argument| self.expr_types[argument.0 as usize])
                .collect();
            return Ok(Some((selected_args, selected_types, slots)));
        }
        if let Some((error, candidate)) = take_unanimous_mapping_error(&mut failures) {
            self.report_callable_arg_mapping_error(
                call,
                args,
                DiagnosticFunction {
                    name,
                    params: &candidate.callable.params,
                    param_names: &candidate.call_sig.param_names,
                    param_defaults: &candidate.call_sig.param_defaults,
                    required: candidate.call_sig.required,
                    vararg: candidate.call_sig.vararg,
                    context_count: candidate.context_count,
                    ret: candidate.callable.ret,
                    source_display: self.module_source_display(&candidate, candidate.callable.ret),
                },
                error,
            );
            let explicit_type_args = self.resolved_explicit_type_args(scope, call);
            self.report_inapplicable_callable_candidates(
                InapplicableTopLevelCall {
                    call,
                    name,
                    args,
                    argument_names: Some(names),
                    trailing_lambda,
                    mapping_error_reported: true,
                    explicit_type_args,
                },
                diagnostic_candidates,
            );
            return Err(());
        }
        if !failures.is_empty() {
            if !self.call_already_has_argument_diagnostic(call, args) {
                self.diags.error(
                    self.call_callee_name_span(call),
                    INAPPLICABLE_OVERLOAD_PREFIX.to_string(),
                );
            }
            return Err(());
        }
        Ok(None)
    }
}
