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

    /// Whether `expression` is the call value argument being checked against its parameter.
    pub(super) fn is_call_value_argument(&self, expression: ExprId) -> bool {
        self.checked_call_argument == Some(expression)
    }
}
