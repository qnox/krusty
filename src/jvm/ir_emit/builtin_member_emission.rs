//! JVM realization of backend-neutral builtin member operations.

use super::*;

impl Emitter<'_> {
    pub(super) fn emit_builtin_member(
        &mut self,
        expression: crate::ir::ExprId,
        operation: &crate::ir::IrIntrinsic,
        receiver: crate::ir::ExprId,
        arguments: &[crate::ir::ExprId],
        code: &mut CodeBuilder,
    ) {
        match operation {
            crate::ir::IrIntrinsic::ArraySize => {
                self.emit_value(receiver, code);
                // `Array<T>.size` receives `[Ljava/lang/Object;`. kotlinc coerces the receiver to
                // that type, which casts an array of arrays, whose dimensions differ.
                if type_descriptor(self.value_ty(receiver)).starts_with("[[") {
                    let object_array = self.cw.class_ref("[Ljava/lang/Object;");
                    code.checkcast(object_array);
                }
                code.arraylength();
            }
            crate::ir::IrIntrinsic::StringGet | crate::ir::IrIntrinsic::StringLength => {
                self.emit_string_member(expression, operation, receiver, arguments, code)
            }
            crate::ir::IrIntrinsic::EnumName => {
                self.emit_value(receiver, code);
                self.mark_dispatch_line(expression, code);
                // kotlinc names the receiver's checked static classifier in the Methodref. JVM
                // lookup then finds the inherited `Enum.name()` implementation. The receiver type
                // is already in its physical JVM form here; no source spelling is reconstructed.
                let owner = self
                    .value_ty(receiver)
                    .obj_internal()
                    .expect("checked Enum.name receiver has a JVM classifier")
                    .render();
                let method = self.cw.methodref(&owner, "name", "()Ljava/lang/String;");
                code.invokevirtual(method, 0, 1);
            }
            crate::ir::IrIntrinsic::NullableAnyToString => {
                self.emit_string_conversion(expression, receiver, code)
            }
            _ => unreachable!("{operation:?} is not a builtin member operation"),
        }
    }

    /// `String.get` (`charAt`) or `String.length` (`length`) on `receiver`. `charAt` is a real
    /// dispatch, so a multi-line call's own line returns before it, as kotlinc marks it.
    fn emit_string_member(
        &mut self,
        expression: crate::ir::ExprId,
        operation: &crate::ir::IrIntrinsic,
        receiver: crate::ir::ExprId,
        arguments: &[crate::ir::ExprId],
        code: &mut CodeBuilder,
    ) {
        self.emit_value(receiver, code);
        match operation {
            crate::ir::IrIntrinsic::StringGet => {
                self.emit_value(arguments[0], code);
                self.mark_dispatch_line(expression, code);
                let method = self.cw.methodref("java/lang/String", "charAt", "(I)C");
                code.invokevirtual(method, 1, 1);
            }
            crate::ir::IrIntrinsic::StringLength => {
                let method = self.cw.methodref("java/lang/String", "length", "()I");
                code.invokevirtual(method, 0, 1);
            }
            _ => unreachable!("{operation:?} is not a String member"),
        }
    }

    /// A value's string conversion (`Any?.toString()`, or `toString()` on a primitive): kotlinc's
    /// one-argument string concatenation, `String.valueOf` overloaded by the value's JVM type (a
    /// `Byte`/`Short` widened to `int`). The call marks its own line, as kotlinc's `visitCall` does.
    fn emit_string_conversion(
        &mut self,
        expression: crate::ir::ExprId,
        receiver: crate::ir::ExprId,
        code: &mut CodeBuilder,
    ) {
        let semantic = self.value_ty(receiver);
        if semantic == Ty::Unit {
            // A Unit expression leaves no value. Materialize its semantic singleton through the
            // ordinary reference-consumer adapter, then dispatch the selected `toString` exactly
            // as kotlinc does.
            let unit = Ty::obj("kotlin/Unit");
            self.emit_value_as(receiver, unit, code);
            self.mark_dispatch_line(expression, code);
            let method = self
                .cw
                .methodref("kotlin/Unit", "toString", "()Ljava/lang/String;");
            code.invokevirtual(method, 0, 1);
            return;
        }
        self.emit_value(receiver, code);
        let descriptor = match semantic {
            Ty::Int | Ty::Short | Ty::Byte => "(I)Ljava/lang/String;",
            Ty::Long => "(J)Ljava/lang/String;",
            Ty::Boolean => "(Z)Ljava/lang/String;",
            Ty::Char => "(C)Ljava/lang/String;",
            Ty::Double => "(D)Ljava/lang/String;",
            Ty::Float => "(F)Ljava/lang/String;",
            _ => "(Ljava/lang/Object;)Ljava/lang/String;",
        };
        self.mark_dispatch_line(expression, code);
        let method = self.cw.methodref("java/lang/String", "valueOf", descriptor);
        code.invokestatic(method, slot_words(semantic) as i32, 1);
    }
}
