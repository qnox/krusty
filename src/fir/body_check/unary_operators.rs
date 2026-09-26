//! Checked FIR for Kotlin's built-in unary operators on primitive values.

use super::*;

impl BodyFirChecker<'_> {
    /// The operand of a built-in `-x`, `+x`, or `!x`. The selected operator is a member of the
    /// primitive type itself, so its receiver is the operand's non-null type: a platform-typed
    /// value (`list[0]` on a Java list) is consumed at that primitive, exactly as a binary
    /// operand is consumed at its operation type.
    pub(super) fn builtin_unary_operand(
        &mut self,
        operand: ExprId,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let actual = self.expression_type(operand)?;
        let receiver = actual.get().non_null();
        if receiver == actual.get() {
            return self.expression(operand);
        }
        let receiver = self.resolved_type(
            self.file
                .expr_span(operand)
                .expect("a checked unary operand has a source span"),
            receiver,
        )?;
        self.value_at_selected_boundary(operand, receiver)
    }
}
