//! The result of an inline-return frame, stored from a lambda body nested inside it.
//!
//! An inline expansion leaves through `break` to its frame's label after storing the returned value
//! as the frame's result. A lambda body spliced inside the frame numbers its values separately, so a
//! return in it names the frame ([`IrExpr::SetFrameResult`]) rather than the result's value index.
//! The frame's result declaration records its slot under the frame's label when it is emitted; the
//! store finds it there from whichever value domain it runs in.

use super::*;

impl Emitter<'_> {
    /// Record where the result of an inline-return frame lives, when `declaration` declares one.
    pub(super) fn open_inline_return_frame(&mut self, declaration: ExprId, slot: u16, ty: Ty) {
        if let Some(frame) = self.ir.inline_return_frames.get(&declaration) {
            self.inline_return_frame_results
                .insert(frame.clone(), (slot, ty));
        }
    }

    /// Store `value` as the result of the enclosing inline-return frame `frame`.
    pub(super) fn emit_set_frame_result(
        &mut self,
        frame: &str,
        value: ExprId,
        code: &mut CodeBuilder,
    ) {
        let Some(&(slot, ty)) = self.inline_return_frame_results.get(frame) else {
            self.run.set_emit_error(
                "a nested return stores the result of a frame that was never declared".to_string(),
            );
            return;
        };
        self.emit_value(value, code);
        // Coerced to the result's type as a store in the frame's own body is.
        self.adapt_physical_operand_for(value, self.value_ty(value), ty, code);
        store(ty, slot, code);
    }
}
