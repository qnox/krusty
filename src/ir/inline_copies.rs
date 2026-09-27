//! Which expressions an inline expansion copied from the inline function's own body.
//!
//! kotlinc compiles an inline function once and copies its compiled instructions into each call
//! site; the caller's code generator never maps the signatures those copied instructions name.
//! Common lowering expands a same-module inline call by copying the body's expressions instead, so
//! it records which expressions are such copies and a backend treats them as copied code.

use super::{ExprId, IrFile};

impl IrFile {
    /// Record `copy` as copied from an inline function's own body.
    pub(crate) fn mark_inline_copy(&mut self, copy: ExprId) {
        self.inline_copies.insert(copy);
    }

    /// `expression` now holds code the call site supplied rather than the inline body's.
    pub(crate) fn unmark_inline_copy(&mut self, expression: ExprId) {
        self.inline_copies.remove(&expression);
    }

    /// A clone of an inline copy is itself one: expanding an inline call copies an inline body
    /// that may already hold another expansion.
    pub(super) fn copy_inline_copy_mark(&mut self, source: ExprId, target: ExprId) {
        if self.is_inline_copy(source) {
            self.mark_inline_copy(target);
        }
    }

    pub(crate) fn is_inline_copy(&self, expression: ExprId) -> bool {
        self.inline_copies.contains(&expression)
    }
}
