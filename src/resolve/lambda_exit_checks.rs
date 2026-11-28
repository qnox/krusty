//! The values leaving a labelled lambda, checked against the lambda's resolved result position.

use super::*;

impl Checker<'_> {
    /// Check one value leaving a labelled lambda against that lambda's result position. A fixed
    /// expectation both contextualizes generic inference and validates the produced value at the
    /// return expression itself; after that boundary the contribution has the expected type, so a
    /// diagnosed mismatch cannot cascade into an unrelated aggregate lambda-result error.
    pub(super) fn check_lambda_return_value(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        lambda: ExprId,
    ) -> Ty {
        let Some(expected) = self.lambda_returns.expected_type(lambda) else {
            return self.expr(scope, expression);
        };
        let returned = self.expr_expected(scope, expression, expected);
        let actual =
            self.recorded_expression_type_for_expected(scope, expression, returned, expected);
        self.narrow_platform_value(expected, expression, PlatformNarrowing::Declaration);
        self.expect_assignable(expected, actual, self.span(expression), "return");
        if returned == Ty::Error {
            Ty::Error
        } else {
            expected
        }
    }

    /// Validate a valueless exit at the resolved lambda boundary. A fixed `Unit` or `Unit?` result
    /// preserves Kotlin's implicit Unit value (and nullable widening); ordinary assignability to a
    /// wider type such as `Any` does not turn the source construct into a value-returning exit. Open
    /// results retain `Unit` as an inference contribution, which is what permits a bare exit to make
    /// an otherwise-generic lambda `Unit`.
    pub(super) fn check_lambda_valueless_return(&mut self, lambda: ExprId, span: Span) -> Ty {
        let Some(expected) = self.lambda_returns.expected_type(lambda) else {
            return Ty::Unit;
        };
        if expected != Ty::Error && expected.non_null() != Ty::Unit {
            self.report_assignability_error(expected, Ty::Unit, span, "return");
        }
        expected
    }
}
