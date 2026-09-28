//! What an inline expansion copied from the inline function's own body, and which of the call
//! site's operands that body never reads.
//!
//! kotlinc compiles an inline function once and copies its compiled instructions into each call
//! site; the caller's code generator never maps the signatures those copied instructions name.
//! Common lowering expands a same-module inline call by copying the body's expressions instead, so
//! it records which expressions are such copies and a backend treats them as copied code.
//!
//! An operand the inlined body never reads is still evaluated, but its value has no use. A target
//! decides from that fact whether the value needs materializing at all.

use std::collections::HashSet;

use super::{ExprId, IrFile};

#[derive(Default)]
pub(super) struct InlineExpansions {
    copies: HashSet<ExprId>,
    unread_operands: HashSet<ExprId>,
}

impl IrFile {
    /// Record `copy` as copied from an inline function's own body.
    pub(crate) fn mark_inline_copy(&mut self, copy: ExprId) {
        self.inline_expansions.copies.insert(copy);
    }

    /// `expression` now holds code the call site supplied rather than the inline body's.
    pub(crate) fn unmark_inline_copy(&mut self, expression: ExprId) {
        self.inline_expansions.copies.remove(&expression);
    }

    /// A clone keeps what its source was: an inline copy (expanding an inline call copies an
    /// inline body that may already hold another expansion), or an operand left unread.
    pub(super) fn copy_inline_copy_mark(&mut self, source: ExprId, target: ExprId) {
        if self.is_inline_copy(source) {
            self.mark_inline_copy(target);
        }
        if self.is_unread_inline_operand(source) {
            self.mark_unread_inline_operand(target);
        }
    }

    pub(crate) fn is_inline_copy(&self, expression: ExprId) -> bool {
        self.inline_expansions.copies.contains(&expression)
    }

    /// Record `operand`, passed to an inline expansion, as never read by the inlined body.
    pub(crate) fn mark_unread_inline_operand(&mut self, operand: ExprId) {
        self.inline_expansions.unread_operands.insert(operand);
    }

    pub(crate) fn is_unread_inline_operand(&self, operand: ExprId) -> bool {
        self.inline_expansions.unread_operands.contains(&operand)
    }
}
