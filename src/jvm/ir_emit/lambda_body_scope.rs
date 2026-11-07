//! The outermost scope of an inline lambda's own method.
//!
//! kotlinc compiles an inline lambda to a method whose body block is the method's outermost scope:
//! its locals close at the method's end, after the marker the inliner declares first, while a
//! nested block's locals close where that block ends, ahead of the marker. A lambda that returns
//! locally is prepared as an inline-return frame around its body, so the body's block is nested in
//! the frame and closes where the returns join. Its locals still belong to the outermost scope:
//! they are held here when the block closes and recorded after the marker.

use super::block_scope::OpenLocal;
use super::{CodeBuilder, Emitter};
use crate::ir::ExprId;

/// The body block of the inline lambda whose method is being compiled, and the locals it closed.
#[derive(Default)]
pub(super) struct LambdaBodyScope {
    /// The body's block, once the lambda's template is known to wrap it in a frame.
    root: Option<ExprId>,
    /// The block depth the body's block opened at.
    depth: Option<usize>,
    /// The body's own locals, closed with their ranges ended, in declaration order.
    closed: Vec<OpenLocal>,
}

impl LambdaBodyScope {
    /// The scope of a lambda whose template is `template`: the body a local return frame wraps.
    pub(super) fn of_template(ir: &crate::ir::IrFile, template: ExprId) -> Self {
        Self {
            root: ir.inline_return_frame_body(template),
            depth: None,
            closed: Vec::new(),
        }
    }

    /// The body's locals, ended where its block closed, to record after the marker.
    pub(super) fn into_closed(self) -> Vec<OpenLocal> {
        self.closed
    }
}

impl Emitter<'_> {
    /// Note that `block` opened at the current depth, when it is the lambda body's block.
    pub(super) fn note_lambda_body_block(&mut self, block: ExprId) {
        let scope = &mut self.lambda_body_scope;
        if scope.root == Some(block) && scope.depth.is_none() {
            scope.depth = Some(self.block_depth);
        }
    }

    /// Hold `local`, closing with the block at `depth`, when that block is the lambda body's: it
    /// ends at `end` and is recorded after the marker. `None` when the caller records it.
    pub(super) fn hold_lambda_body_local(
        &mut self,
        depth: usize,
        mut local: OpenLocal,
        end: u16,
    ) -> Option<OpenLocal> {
        if self.lambda_body_scope.depth != Some(depth) || local.depth != depth {
            return Some(local);
        }
        local.explicit_end.get_or_insert(end);
        self.lambda_body_scope.closed.push(local);
        None
    }
}

/// Record the lambda body's held `locals` after its marker, through the method's end `end` unless
/// they ended earlier.
pub(super) fn record_body_locals(locals: Vec<OpenLocal>, end: u16, code: &mut CodeBuilder) {
    for local in locals {
        let length = local
            .explicit_end
            .unwrap_or(end)
            .saturating_sub(local.start);
        local.record(Some(length), code);
    }
}
