//! The blocks that are a callable's body, as kotlinc's `IrBlockBody` holds its statements.
//!
//! A local declared at the top of a body stays in scope until the method ends, through the
//! implicit return that follows the block, as kotlinc's `LocalVariableTable` records it. Common
//! lowering marks the block it wrapped the body's statements in; a backend asks instead of inferring
//! the body from the shape around it.

use std::collections::HashSet;

use super::{ExprId, IrFile};

#[derive(Default)]
pub(super) struct BodyScopes {
    blocks: HashSet<ExprId>,
}

impl IrFile {
    pub(crate) fn mark_body_scope(&mut self, block: ExprId) {
        self.body_scopes.blocks.insert(block);
    }

    /// Whether `block` holds a callable body's own statements, whose locals the method's end
    /// closes rather than the block's.
    pub(crate) fn is_body_scope(&self, block: ExprId) -> bool {
        self.body_scopes.blocks.contains(&block)
    }
}
