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

use std::collections::{HashMap, HashSet};

use super::{ExprId, IrFile};
use crate::types::TypeName;

/// Source identity for code copied from a same-file inline declaration. Common IR retains the
/// semantic owner and call line; a target formats the physical source-map path and line range.
#[derive(Clone, Copy, Debug)]
pub(crate) struct IrInlineCopyProvenance {
    pub(crate) owner: Option<TypeName>,
    pub(crate) call_line: Option<u32>,
}

#[derive(Default)]
pub(super) struct InlineExpansions {
    copies: HashSet<ExprId>,
    unread_operands: HashSet<ExprId>,
    provenance: HashMap<ExprId, IrInlineCopyProvenance>,
}

impl IrFile {
    /// Record `copy` as copied from an inline function's own body.
    pub(crate) fn mark_inline_copy(&mut self, copy: ExprId) {
        self.inline_expansions.copies.insert(copy);
    }

    /// Record the declaration owner whose source body `copy` came from. An already-provenanced
    /// nested inline copy keeps its inner declaration rather than being relabelled as the outer.
    pub(crate) fn record_inline_copy_owner(&mut self, copy: ExprId, owner: Option<TypeName>) {
        self.mark_inline_copy(copy);
        self.inline_expansions
            .provenance
            .entry(copy)
            .or_insert(IrInlineCopyProvenance {
                owner,
                call_line: None,
            });
    }

    /// Attach the source call line once the containing FIR expression has supplied it.
    pub(crate) fn record_inline_copy_call_line(&mut self, copy: ExprId, line: u32) {
        if line == 0 {
            return;
        }
        if let Some(provenance) = self.inline_expansions.provenance.get_mut(&copy) {
            provenance.call_line.get_or_insert(line);
        }
    }

    pub(crate) fn inline_copy_provenance(
        &self,
        expression: ExprId,
    ) -> Option<IrInlineCopyProvenance> {
        self.inline_expansions.provenance.get(&expression).copied()
    }

    /// `expression` now holds code the call site supplied rather than the inline body's.
    pub(crate) fn unmark_inline_copy(&mut self, expression: ExprId) {
        self.inline_expansions.copies.remove(&expression);
        self.inline_expansions.provenance.remove(&expression);
    }

    /// A clone keeps what its source was: an inline copy (expanding an inline call copies an
    /// inline body that may already hold another expansion), or an operand left unread.
    pub(crate) fn copy_inline_copy_mark(&mut self, source: ExprId, target: ExprId) {
        if self.is_inline_copy(source) {
            self.mark_inline_copy(target);
        }
        if let Some(provenance) = self.inline_copy_provenance(source) {
            self.inline_expansions.provenance.insert(target, provenance);
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

    pub(crate) fn remap_inline_copy_owners(
        &mut self,
        names: &std::collections::HashMap<TypeName, TypeName>,
    ) {
        for provenance in self.inline_expansions.provenance.values_mut() {
            if let Some(owner) = &mut provenance.owner {
                *owner = names.get(owner).copied().unwrap_or(*owner);
            }
        }
    }
}
