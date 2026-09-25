//! Emission of kotlinc's codegen `InlineMarker` calls, as krusty's stand-in instruction (see
//! [`CodegenMarker`]). Unlike the coroutine-site markers these are not erased when the method is
//! finished: they stay in the body until the coroutine transformer reads and removes them when the
//! class is written, exactly as kotlinc's transformer removes the calls its codegen left.

use super::{ClassWriter, CodeBuilder};
use crate::jvm::bytecode::{CodegenMarker, CODEGEN_MARKER_OP};

impl CodeBuilder {
    /// `InlineMarker.beforeInlineCall()` or `afterInlineCall()`.
    pub fn inline_call_marker(&mut self, before: bool) {
        let marker = match before {
            true => CodegenMarker::BeforeInlineCall,
            false => CodegenMarker::AfterInlineCall,
        };
        self.op_u1(CODEGEN_MARKER_OP, marker as u8, 0);
    }

    /// `InlineMarker.mark(id)`: the id as an ordinary constant, then the call that takes it.
    pub fn suspend_marker(&mut self, id: i32, cw: &mut ClassWriter) {
        self.push_int(id, cw);
        self.op_u1(CODEGEN_MARKER_OP, CodegenMarker::Mark as u8, -1);
    }
}
