//! Operand sequences that deliberately remain live across nested StackMapTable frames.

use super::*;

impl Emitter<'_> {
    /// Emit `e` while `held` stack entries are already below it. Nested merge frames must describe
    /// the complete verifier stack rather than forcing the caller to spill those operands.
    pub(super) fn emit_value_over(
        &mut self,
        expression: u32,
        held: &[VerifType],
        code: &mut CodeBuilder,
    ) {
        if held.is_empty() || !self.records_frame(expression) {
            self.emit_value(expression, code);
            return;
        }
        let outer_depth = self.pending_stack.len();
        self.pending_stack.extend_from_slice(held);
        self.emit_value(expression, code);
        self.pending_stack.truncate(outer_depth);
    }

    /// Preserve a completed left operand across a frame-producing right operand. This is safe when
    /// the right side has no exception handler or inline splice, both of which require an empty
    /// operand baseline. It avoids artificial locals for JVM integer/long bit operations.
    pub(super) fn emit_binary_operands_over_frames(
        &mut self,
        lhs: u32,
        rhs: u32,
        lhs_ty: Ty,
        code: &mut CodeBuilder,
    ) {
        if self.records_frame(rhs) && !self.must_spill_across(rhs) {
            self.emit_value(lhs, code);
            let held = self.verif_single(lhs_ty);
            self.emit_value_over(rhs, &[held], code);
        } else {
            self.emit_operands(&[lhs, rhs], code);
        }
    }
}
