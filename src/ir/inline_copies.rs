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
use crate::types::{Ty, TypeName};

/// Source identity for code copied from a same-module inline declaration. Common IR retains the
/// exact callable identity, semantic owner, the callee's source file, and the call line; a target
/// formats the physical source-map path and line range.
#[derive(Clone, Copy, Debug)]
pub(crate) struct IrInlineCopyProvenance {
    /// Exact common-IR declaration whose body was copied. A backend uses this identity to obtain
    /// an already-resolved physical realization; it must not reconstruct one from source names.
    pub(crate) function: super::FunId,
    pub(crate) owner: Option<TypeName>,
    /// The source file that declares the inline function.
    pub(crate) source: crate::fir::SourceFileId,
    pub(crate) call_line: Option<u32>,
}

#[derive(Default)]
pub(super) struct InlineExpansions {
    copies: HashSet<ExprId>,
    unread_operands: HashSet<ExprId>,
    provenance: HashMap<ExprId, IrInlineCopyProvenance>,
    /// The semantic type of a materialized inline declaration before call-site specialization.
    /// Operands, unnamed generic temporaries, and result frames keep this provenance while their
    /// expressions carry the specialized semantic type. A backend may use the declaration type
    /// for physical storage and debug representation.
    declared_inline_types: HashMap<ExprId, Ty>,
    /// Discarded copied `Unit` values whose source closing line must remain physically anchored.
    /// Other copied `Unit` nodes carry source-map provenance without inventing an instruction.
    retained_unit_lines: HashSet<ExprId>,
}

impl IrFile {
    /// Record `copy` as copied from an inline function's own body.
    pub(crate) fn mark_inline_copy(&mut self, copy: ExprId) {
        self.inline_expansions.copies.insert(copy);
    }

    /// Record the declaration owner and source file whose body `copy` came from. An
    /// already-provenanced nested inline copy keeps its inner declaration rather than being
    /// relabelled as the outer.
    pub(crate) fn record_inline_copy_owner(
        &mut self,
        copy: ExprId,
        function: super::FunId,
        owner: Option<TypeName>,
        source: crate::fir::SourceFileId,
    ) {
        self.mark_inline_copy(copy);
        self.inline_expansions
            .provenance
            .entry(copy)
            .or_insert(IrInlineCopyProvenance {
                function,
                owner,
                source,
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
        self.inline_expansions
            .retained_unit_lines
            .remove(&expression);
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
        if let Some(ty) = self.inline_declared_type(source) {
            self.record_inline_declared_type(target, ty);
        }
        if self.retains_inline_unit_line(source) {
            self.retain_inline_unit_line(target);
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

    pub(crate) fn record_inline_declared_type(&mut self, declaration: ExprId, ty: Ty) {
        self.inline_expansions
            .declared_inline_types
            .insert(declaration, ty);
    }

    pub(crate) fn inline_declared_type(&self, declaration: ExprId) -> Option<Ty> {
        self.inline_expansions
            .declared_inline_types
            .get(&declaration)
            .copied()
    }

    pub(crate) fn inline_declared_types(&self) -> impl Iterator<Item = Ty> + '_ {
        self.inline_expansions
            .declared_inline_types
            .values()
            .copied()
    }

    pub(crate) fn inline_declared_types_mut(&mut self) -> impl Iterator<Item = &mut Ty> {
        self.inline_expansions.declared_inline_types.values_mut()
    }

    pub(crate) fn inline_declared_type_mut(&mut self, declaration: ExprId) -> Option<&mut Ty> {
        self.inline_expansions
            .declared_inline_types
            .get_mut(&declaration)
    }

    pub(crate) fn retain_inline_unit_line(&mut self, expression: ExprId) {
        self.inline_expansions
            .retained_unit_lines
            .insert(expression);
    }

    pub(crate) fn retains_inline_unit_line(&self, expression: ExprId) -> bool {
        self.inline_expansions
            .retained_unit_lines
            .contains(&expression)
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
