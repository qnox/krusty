//! Rebinding reparsed declaration annotations to finalized semantic types.

use crate::ast::TypeRef;
use crate::types::Ty;

use super::Checker;

impl Checker<'_> {
    /// Rebind a reparsed value-parameter annotation to the semantic type finalized in Pass 1.
    /// Signatures store a vararg's callable-facing array type, while the annotation occurrence
    /// denotes its element type; record that syntax-facing view and return the callable-facing type
    /// used for the lexical parameter binding.
    pub(super) fn record_finalized_parameter_type(
        &mut self,
        reference: &TypeRef,
        is_vararg: bool,
        semantic: Ty,
    ) -> Ty {
        let declaration = if is_vararg {
            semantic.non_null().array_read_elem().unwrap_or(semantic)
        } else {
            semantic
        };
        self.resolved_type_tys
            .insert((reference.span.lo, reference.span.hi), declaration);
        self.resolved_declaration_types
            .insert((reference.span.lo, reference.span.hi), declaration);
        semantic
    }

    /// Rebind an ordinary declaration annotation to the semantic type selected in Pass 1.
    /// `check_declaration_type` publishes both occurrence and declaration channels; the finalized
    /// path must preserve that same `TypeInfo` contract without repeating lookup.
    pub(super) fn record_finalized_declaration_type(
        &mut self,
        reference: &TypeRef,
        semantic: Ty,
    ) -> Ty {
        self.resolved_type_tys
            .insert((reference.span.lo, reference.span.hi), semantic);
        self.resolved_declaration_types
            .insert((reference.span.lo, reference.span.hi), semantic);
        semantic
    }
}
