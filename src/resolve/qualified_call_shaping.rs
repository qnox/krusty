//! Reshaping a selected call's lambda arguments, and retiring the probe that preceded it.
//!
//! A call's arguments must be typed before a candidate can be chosen, so a lambda is first judged
//! with no expected shape: a receiver lambda has no receiver to resolve against and reports
//! unresolved references for members that are perfectly real. Those diagnostics are held aside, the
//! lambdas are rechecked against the parameters the SELECTED callable declares, and only then is the
//! probe's account of them dropped.
//!
//! The two halves belong together — rechecking without retiring the probe double-reports, and
//! retiring without rechecking loses the shape — which is why they live in one operation here rather
//! than inline at the call site.

use super::*;

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

    /// Recheck every lambda argument against its selected parameter, then discard exactly the probe
    /// diagnostics those lambdas produced.
    ///
    /// A parameter carries its receiver on either channel: the call-sig's per-parameter mark, or the
    /// function type itself, since an extension's call-sig has no mark by design.
    pub(super) fn reshape_selected_lambda_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
        arg_tys: &mut [Ty],
        selected: &SelectedCallable,
        probe_mark: usize,
    ) {
        let label = call_implicit_lambda_label(self.file, call).map(str::to_string);
        let arg_names = self.file.call_arg_names.get(&call.0);
        for (index, &argument) in args.iter().enumerate() {
            if !matches!(self.file.expr(argument), Expr::Lambda { .. }) {
                continue;
            }
            // A NAMED argument names its parameter; its source position says nothing about which one
            // it fills. Two reordered function-typed parameters would otherwise be checked against
            // each other's receivers, rejecting a call the reference compiler accepts.
            let slot = arg_names
                .and_then(|names| names.get(index).cloned().flatten())
                .map(|name| {
                    selected
                        .call_sig
                        .param_names
                        .iter()
                        .position(|declared| *declared == name)
                })
                .unwrap_or(Some(index));
            let Some(slot) = slot else {
                continue;
            };
            let Some(&parameter) = selected.callable.params.get(slot) else {
                continue;
            };
            if !matches!(parameter.non_null(), Ty::Fun(_)) {
                continue;
            }
            let has_receiver = selected
                .call_sig
                .lambda_receivers
                .get(slot)
                .is_some_and(Option::is_some)
                || matches!(
                    parameter.non_null(),
                    Ty::Fun(signature) if signature.has_receiver
                );
            let checked =
                self.with_lambda_mutation(selected.callable.inline.can_inline(), |checker| {
                    checker.check_argument_expected(
                        scope,
                        argument,
                        parameter,
                        has_receiver,
                        label.as_deref(),
                    )
                });
            if let Some(slot) = arg_tys.get_mut(index) {
                *slot = checked;
            }
        }
        // The probe's LAMBDA diagnostics described a body with no shape; the recheck above superseded
        // them. Everything else in the batch — an ordinary argument that does not resolve, for
        // instance — is authoritative and must still reach the user, so only the lambda spans are
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
}
