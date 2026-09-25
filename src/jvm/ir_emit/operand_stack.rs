//! Operand sequences that deliberately retain a live stack prefix across nested control flow.

use super::*;

impl Emitter<'_> {
    /// Preserve a completed left operand across a branchy right operand. This is safe when
    /// the right side has no exception handler, suspension, or transfer to an enclosing loop.
    /// Ordinary inline splice branches preserve the prefix through final-body dataflow. This avoids
    /// artificial locals for JVM integer/long bit operations.
    pub(super) fn emit_binary_operands_with_live_prefix(
        &mut self,
        lhs: u32,
        rhs: u32,
        code: &mut CodeBuilder,
    ) {
        if self.emits_control_flow(rhs) && !self.must_spill_across(rhs) {
            self.emit_value(lhs, code);
            self.emit_value(rhs, code);
        } else {
            self.emit_operands(&[lhs, rhs], code);
        }
    }
}
