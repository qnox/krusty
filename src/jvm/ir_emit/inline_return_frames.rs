//! The result of an inline-return frame, stored from a lambda body nested inside it.
//!
//! An inline expansion leaves through `break` to its frame's label after storing the returned value
//! as the frame's result. A lambda body spliced inside the frame numbers its values separately, so a
//! return in it names the frame ([`IrExpr::SetFrameResult`]) rather than the result's value index.
//! The frame's result declaration records its value identity under the frame's label when it is
//! emitted; the first store activates that value's deferred JVM slot from whichever value domain
//! it runs in.

use super::*;

impl Emitter<'_> {
    /// Record the deferred result of an inline-return frame, when `declaration` declares one.
    pub(super) fn open_inline_return_frame(
        &mut self,
        declaration: ExprId,
        value: u32,
        ty: Ty,
    ) -> bool {
        if let Some(frame) = self.ir.inline_return_frames.get(&declaration) {
            self.inline_return_frame_results
                .insert(frame.clone(), (value, ty));
            return true;
        }
        false
    }

    pub(super) fn inline_return_frame_result_type(&self, value: u32) -> Option<Ty> {
        self.inline_return_frame_results
            .values()
            .find_map(|&(candidate, ty)| (candidate == value).then_some(ty))
    }

    /// Claim a deferred result slot after the expression producing its first value has closed its
    /// nested local scopes. Ordinary locals already occupy their frame entry and keep their slot.
    pub(super) fn activate_inline_return_frame_result(&mut self, value: u32, ty: Ty) -> u16 {
        if self.inline_return_frame_result_type(value).is_none() {
            return self.slots[&value].0;
        }
        let slot = self
            .frame
            .activate_deferred(super::frame_map::FrameKey::Value(value));
        self.slots.insert(value, (slot, ty));
        slot
    }

    /// Store `value` as the result of the enclosing inline-return frame `frame`.
    pub(super) fn emit_set_frame_result(
        &mut self,
        frame: &str,
        value: ExprId,
        code: &mut CodeBuilder,
    ) {
        let Some(&(result, ty)) = self.inline_return_frame_results.get(frame) else {
            self.run.set_emit_error(
                "a nested return stores the result of a frame that was never declared".to_string(),
            );
            return;
        };
        self.emit_value(value, code);
        // Coerced to the result's type as a store in the frame's own body is.
        self.adapt_physical_operand_for(value, self.value_ty(value), ty, code);
        let slot = self.activate_inline_return_frame_result(result, ty);
        store(ty, slot, code);
        self.unassigned_values.remove(&result);
    }
}
