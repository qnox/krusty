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
        declaration: crate::ir::ExprId,
        code: &mut CodeBuilder,
    ) {
        let slot = self
            .frame
            .enter(FrameKey::InlineFrameMarker(declaration), Ty::Int);
        code.push_int(0, self.cw);
        store(Ty::Int, slot, code);
        if !self.record_locals {
            return;
        }
        if super::super::debug_local_names::declared_name(self.ir, declaration).is_none() {
            self.run.set_emit_error(
                "an inline frame marker is missing the provenance its name requires".to_string(),
            );
            return;
        }
        self.open_declared_local(declaration, slot, Ty::Int, code);
    }
}
