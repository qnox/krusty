//! JVM emission of the `String` members checked IR keeps as intrinsics: `get` and `length` are
//! the `java/lang/String` methods `charAt` and `length`.

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
}
