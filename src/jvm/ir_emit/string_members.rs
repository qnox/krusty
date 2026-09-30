//! JVM emission of the `String` members checked IR keeps as intrinsics: `get` and `length` are
//! the `java/lang/String` methods `charAt` and `length`; a value's string conversion is
//! `String.valueOf`.

use super::*;

impl Emitter<'_> {
    /// `String.get` (`charAt`) or `String.length` (`length`) on `receiver`. `charAt` is a real
    /// dispatch, so a multi-line call's own line returns before it, as kotlinc marks it.
    pub(super) fn emit_string_member(
        &mut self,
        expression: u32,
        operation: &crate::ir::IrIntrinsic,
        receiver: u32,
        arguments: &[u32],
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
    pub(super) fn emit_string_conversion(
        &mut self,
        expression: u32,
        receiver: u32,
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
