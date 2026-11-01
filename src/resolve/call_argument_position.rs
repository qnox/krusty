//! Whether an expression stands directly as a call value argument. kotlinc postpones such an
//! operand into its call, so a failure reports the call's candidates; the same operand elsewhere is
//! checked against its expected type only after it resolves.

use super::*;

impl Checker<'_> {
    /// Check the call value argument `expression` against its parameter type `expected`.
    pub(super) fn check_argument_expected(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        expected: Ty,
        has_receiver: bool,
        label: Option<&str>,
    ) -> Ty {
        let outer = self.checked_call_argument.replace(expression);
        let ty = self.check_expected_operand(scope, expression, expected, has_receiver, label);
        self.checked_call_argument = outer;
        ty
    }

    /// Check the call or safe call `call`, its value arguments included.
    pub(super) fn checking_call<T>(
        &mut self,
        call: ExprId,
        check: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.checked_calls.push(call);
        let checked = check(self);
        self.checked_calls.pop();
        checked
    }

    /// Whether `expression` is a value argument of a call being checked, or the argument being
    /// committed to its parameter (a constructor delegation's or an enum entry's).
    pub(super) fn is_call_value_argument(&self, expression: ExprId) -> bool {
        self.checked_call_argument == Some(expression)
            || self
                .checked_calls
                .iter()
                .any(|&call| match self.file.expr(call) {
                    Expr::Call { args, .. }
                    | Expr::SafeCall {
                        args: Some(args), ..
                    } => args.contains(&expression),
                    _ => false,
                })
    }
}
