//! The `Intrinsics` call a `!!` or a platform-value assertion writes over the value on the stack.

use super::Emitter;
use krusty::ir::{ExprId, NullCheck};
use krusty::jvm::classfile::CodeBuilder;

impl Emitter<'_> {
    /// Check the reference on top of the stack against null, leaving it in place.
    pub(super) fn emit_null_assertion_check(
        &mut self,
        assertion: ExprId,
        check: &NullCheck,
        code: &mut CodeBuilder,
    ) {
        // kotlinc lowers the implicit check to a temporary of the checked value
        // (`TypeOperatorLowering`), whose declaration marks the check's line once the
        // value is on the stack: after an inlined operand the mapper wrote nothing yet.
        if matches!(check, NullCheck::Named { .. }) {
            self.mark_expression_start(assertion, code);
        }
        code.dup();
        // A platform value narrowed to a declared non-null type names the checked expression
        // in its failure (`getenv(...) must not be null`); `x!!` has no such name and uses the
        // one-argument form. Both consume the duplicate and leave the value in place.
        let m = match check {
            NullCheck::Named { message, .. } => {
                code.push_string(message, self.cw);
                self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullExpressionValue",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                )
            }
            NullCheck::Source | NullCheck::Unnamed { .. } => self.cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNull",
                "(Ljava/lang/Object;)V",
            ),
        };
        let arguments = if matches!(check, NullCheck::Named { .. }) {
            2
        } else {
            1
        };
        code.invokestatic(m, arguments, 0);
    }
}
