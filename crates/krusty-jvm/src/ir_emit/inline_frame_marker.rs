//! The JVM local an inline-expansion frame boundary occupies.
//!
//! Common IR records the boundary and its provenance. This pass materializes kotlinc's
//! `iconst_0; istore` and, when debug locals are recorded, the table name. The slot is not a
//! semantic value, so a coroutine spill plan does not recover it from the value-index space.

use super::frame_map::FrameKey;
use super::*;

impl Emitter<'_> {
    pub(super) fn emit_inline_frame_marker(
        &mut self,
        declaration: krusty::ir::ExprId,
        code: &mut CodeBuilder,
    ) {
        let pending = self
            .copied_frames
            .open
            .pop_if(|frame| frame.block_depth == self.block_depth);
        let default = pending.as_ref().and_then(|frame| frame.default);
        if let Some(enclosing) = pending.as_ref().map(|frame| frame.enclosing_peak) {
            let reached = self.frame.end_peak(enclosing);
            if default.is_some() {
                self.frame.reserve_through(reached);
            }
        }
        let frame_start = pending
            .and_then(|frame| frame.start)
            .or_else(|| u16::try_from(code.bytes.len()).ok())
            .unwrap_or(u16::MAX);
        let slot = self
            .frame
            .enter(FrameKey::InlineFrameMarker(declaration), Ty::Int);
        if let Some(default) = default {
            // The stub calls the real body on the default's line: an inline call evaluated inside
            // the default left no line in effect, so the call writes it again.
            if code.line_forgotten() {
                if let Some(line) = self.ir.source_line(default) {
                    self.mark_expression_line(default, line, code);
                }
            }
        }
        code.push_int(0, self.cw);
        store(Ty::Int, slot, code);
        if default.is_some() {
            code.forget_line();
        }
        if !self.record_locals {
            return;
        }
        if super::super::debug_local_names::declared_name(self.ir, declaration).is_none() {
            self.run.set_emit_error(
                "an inline frame marker is missing the provenance its name requires".to_string(),
            );
            return;
        }
        let parent = self.active_inline_frame_identity();
        let Some(frame) = self.open_declared_local(declaration, slot, Ty::Int, code) else {
            return;
        };
        self.open_inline_scope_frame(declaration, frame, parent, code);
        // Operands are evaluated and opened before the frame marker, but semantically belong to
        // the frame it starts. Bind both their range start and their table-order identity here.
        // Only this expansion's own operands are bound: an operand already bound while a later
        // operand runs an inline call of its own belongs to the enclosing block, not that call.
        let depth = self.block_depth;
        for local in &mut self.open_locals {
            if local.inline_operand && local.inline_frame.is_none() && local.depth == depth {
                // A default evaluated inside the frame starts its range after its own store.
                if local.start <= frame_start {
                    local.start = frame_start;
                }
                local.inline_frame = Some(frame);
            }
        }
    }
}
