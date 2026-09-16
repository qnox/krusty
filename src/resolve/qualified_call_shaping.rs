//! Reshaping a selected call's lambda arguments, and retiring the probe that preceded it.
//!
//! A call's arguments must be typed before a candidate can be chosen, so a lambda is first judged
//! with no expected shape: a receiver lambda has no receiver to resolve against and reports
//! unresolved references for members that are perfectly real. Those diagnostics are held aside, the
//! probe's account of them is dropped, and the shared selected-argument commit rechecks the lambdas
//! against the parameters the selected callable declares.
//!
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
}
