//! Operand sequences that deliberately remain live across nested StackMapTable frames.

use super::*;

impl Emitter<'_> {
    /// Preserve a completed left operand across a frame-producing right operand. This is safe when
    /// the right side has no exception handler or inline splice, both of which require an empty
    /// operand baseline. It avoids artificial locals for JVM integer/long bit operations.
    pub(super) fn emit_binary_operands_over_frames(
        &mut self,
        lhs: u32,
        rhs: u32,
        code: &mut CodeBuilder,
    ) {
        if self.records_frame(rhs) && !self.must_spill_across(rhs) {
            self.emit_value(lhs, code);
            self.emit_value(rhs, code);
        } else {
            self.emit_operands(&[lhs, rhs], code);
        }
    }
}
