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

use crate::fx_hash::{HashMap, HashSet};

use super::expression_copy::InheritedFacts;
use super::{ExprId, IrFile};
use crate::types::{Ty, TypeName};

/// Source identity for code copied from a same-module inline declaration. Common IR retains the
/// exact callable identity, semantic owner, the callee's source file, and the call line; a target
/// formats the physical source-map path and line range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrInlineCopyProvenance {
    /// Exact common-IR declaration whose body was copied. A backend uses this identity to obtain
    /// an already-resolved physical realization; it must not reconstruct one from source names.
    pub function: super::FunId,
    pub owner: Option<TypeName>,
    /// The source file that declares the inline function.
    pub source: crate::fir::SourceFileId,
    pub call_line: Option<u32>,
}

#[derive(Default)]
pub(super) struct InlineExpansions {
    copies: HashSet<ExprId>,
    unread_operands: HashSet<ExprId>,
    /// Innermost-to-outermost source frames. A same-module body can already contain an inline
    /// expansion when it is copied into another call; retaining only its first frame loses the
    /// containing call and prevents a backend from mapping that nested source boundary.
    provenance: HashMap<ExprId, Vec<IrInlineCopyProvenance>>,
    /// The semantic type of a materialized inline declaration before call-site specialization.
    /// Operands, unnamed generic temporaries, and result frames keep this provenance while their
    /// expressions carry the specialized semantic type. A backend may use the declaration type
    /// for physical storage and debug representation.
    declared_inline_types: HashMap<ExprId, Ty>,
    /// Same-module inline-expansion blocks whose trailing value crosses the copied function's
    /// frame boundary. The copied body produces the physical value; its caller adapts or discards
    /// that value only after the frame's debug locals have ended.
    result_boundaries: HashSet<ExprId>,
    /// Discarded copied `Unit` values whose source closing line must remain physically anchored.
    /// Other copied `Unit` nodes carry source-map provenance without inventing an instruction.
    retained_unit_lines: HashSet<ExprId>,
}

impl IrFile {
    /// Record `copy` as copied from an inline function's own body.
    pub fn mark_inline_copy(&mut self, copy: ExprId) {
        self.inline_expansions.copies.insert(copy);
    }

    /// Record the declaration identity, owner, and source file whose body `copy` came from. An
    /// already-provenanced nested copy keeps its inner frames and appends this containing
    /// expansion.
    pub fn record_inline_copy_owner(
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
            .or_default()
            .push(IrInlineCopyProvenance {
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
        if let Some(provenance) = self
            .inline_expansions
            .provenance
            .get_mut(&copy)
            .and_then(|frames| frames.last_mut())
        {
            provenance.call_line.get_or_insert(line);
        }
    }

    pub fn inline_copy_provenance(&self, expression: ExprId) -> Option<&[IrInlineCopyProvenance]> {
        self.inline_expansions
            .provenance
            .get(&expression)
            .map(Vec::as_slice)
    }

    /// `expression` now holds code the call site supplied rather than the inline body's.
    pub(crate) fn unmark_inline_copy(&mut self, expression: ExprId) {
        self.inline_expansions.copies.remove(&expression);
        self.inline_expansions.provenance.remove(&expression);
        self.inline_expansions.result_boundaries.remove(&expression);
        self.inline_expansions
            .retained_unit_lines
            .remove(&expression);
    }

    /// A clone keeps what its source was: an inline copy (expanding an inline call copies an
    /// inline body that may already hold another expansion), or an operand left unread.
    pub fn copy_inline_copy_mark(&mut self, source: ExprId, target: ExprId) {
        self.inline_expansions.inherit(source, target);
    }

    pub fn is_inline_copy(&self, expression: ExprId) -> bool {
        self.inline_expansions.copies.contains(&expression)
    }

    /// Record `operand`, passed to an inline expansion, as never read by the inlined body.
    pub fn mark_unread_inline_operand(&mut self, operand: ExprId) {
        self.inline_expansions.unread_operands.insert(operand);
    }

    pub fn is_unread_inline_operand(&self, operand: ExprId) -> bool {
        self.inline_expansions.unread_operands.contains(&operand)
    }

    pub fn record_inline_declared_type(&mut self, declaration: ExprId, ty: Ty) {
        self.inline_expansions
            .declared_inline_types
            .insert(declaration, ty);
    }

    pub fn inline_declared_type(&self, declaration: ExprId) -> Option<Ty> {
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

    pub(crate) fn mark_inline_result_boundary(&mut self, expression: ExprId) {
        self.inline_expansions.result_boundaries.insert(expression);
    }

    pub fn is_inline_result_boundary(&self, expression: ExprId) -> bool {
        self.inline_expansions
            .result_boundaries
            .contains(&expression)
    }

    pub(crate) fn retain_inline_unit_line(&mut self, expression: ExprId) {
        self.inline_expansions
            .retained_unit_lines
            .insert(expression);
    }

    pub fn retains_inline_unit_line(&self, expression: ExprId) -> bool {
        self.inline_expansions
            .retained_unit_lines
            .contains(&expression)
    }

    pub(crate) fn remap_inline_copy_owners(
        &mut self,
        names: &crate::fx_hash::HashMap<TypeName, TypeName>,
    ) {
        for provenance in self.inline_expansions.provenance.values_mut() {
            for frame in provenance {
                if let Some(owner) = &mut frame.owner {
                    *owner = names.get(owner).copied().unwrap_or(*owner);
                }
            }
        }
    }
}

impl InheritedFacts for InlineExpansions {
    fn inherit(&mut self, source: ExprId, target: ExprId) {
        let Self {
            copies,
            unread_operands,
            provenance,
            declared_inline_types,
            result_boundaries,
            retained_unit_lines,
        } = self;
        copies.inherit(source, target);
        unread_operands.inherit(source, target);
        // A copy placed in a frame that already has provenance keeps those frames and appends
        // the source's.
        if let Some(frames) = provenance.get(&source).cloned() {
            provenance.entry(target).or_default().extend(frames);
        }
        declared_inline_types.inherit(source, target);
        result_boundaries.inherit(source, target);
        retained_unit_lines.inherit(source, target);
    }

    fn truncate_from(&mut self, first: ExprId) {
        let Self {
            copies,
            unread_operands,
            provenance,
            declared_inline_types,
            result_boundaries,
            retained_unit_lines,
        } = self;
        for facts in [
            copies as &mut dyn InheritedFacts,
            unread_operands,
            provenance,
            declared_inline_types,
            result_boundaries,
            retained_unit_lines,
        ] {
            facts.truncate_from(first);
        }
    }
}
