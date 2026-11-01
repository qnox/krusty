//! The `Intrinsics` call a `!!` or a platform-value assertion writes over the value on the stack.

use super::Emitter;
use crate::ir::NullCheck;
use crate::jvm::classfile::CodeBuilder;

impl Emitter<'_> {
    /// Check the reference on top of the stack against null, leaving it in place.
    pub(super) fn emit_null_assertion_check(&mut self, check: &NullCheck, code: &mut CodeBuilder) {
        // An inlined operand leaves the caller's line forgotten. kotlinc marks the checked
        // expression's line again at the implicit check, so the check owns an entry even though
        // the restored caller line is already in effect.
        if matches!(check, NullCheck::Named(_)) && code.line_forgotten() {
            if let Some(line) = code.current_line() {
                code.mark_line(line.into());
            }
        }
        code.dup();
        // A platform value narrowed to a declared non-null type names the checked expression in its
        // failure (`getenv(...) must not be null`); `x!!` has no such name and uses the
        // one-argument form. Both consume the duplicate and leave the value in place.
        let m = match check {
            NullCheck::Named(message) => {
                code.push_string(message, self.cw);
                self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullExpressionValue",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                )
            }
            NullCheck::Source | NullCheck::Unnamed => self.cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNull",
                "(Ljava/lang/Object;)V",
            ),
        };
        let arguments = if matches!(check, NullCheck::Named(_)) {
            2
        } else {
            1
        };
        code.invokestatic(m, arguments, 0);
    }
}
