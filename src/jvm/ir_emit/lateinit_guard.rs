//! The uninitialized guard on a `lateinit` read.
//!
//! kotlinc's `LateinitLowering` reads the value into a temporary and throws when it is null:
//! `if (tmp == null) throwUninitializedPropertyAccessException("x") else tmp`. Its codegen keeps
//! the read on the stack, so every use site writes
//! `dup; ifnonnull L; pop; ldc "x"; invokestatic throw…; aconst_null; L:` — the `throw` intrinsic
//! returns `void` to the verifier, and the null path pushes the `null` its branch yields. A member
//! field, a static, an access bridge's raw field load and a local variable share this shape.

use super::*;

impl Emitter<'_> {
    /// Guard the `lateinit` value on top of the stack, named `name` in the exception.
    pub(super) fn emit_lateinit_use_guard(&mut self, name: &str, code: &mut CodeBuilder) {
        code.dup();
        let initialized = code.new_label();
        code.ifnonnull(initialized);
        code.pop();
        code.push_string(name, self.cw);
        let throw = self.cw.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "throwUninitializedPropertyAccessException",
            "(Ljava/lang/String;)V",
        );
        code.invokestatic(throw, 1, 0);
        code.aconst_null();
        self.bind(initialized, code);
    }
}
