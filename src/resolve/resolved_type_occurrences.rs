//! Rebinding reparsed declaration annotations to finalized semantic types.

use crate::ast::TypeRef;
use crate::diag::Span;
use crate::types::Ty;

use super::Checker;

impl Checker<'_> {
    /// Bind the type-use annotations of an occurrence while its authoritative lexical scope is in
    /// hand. Only the semantic fact consumed later is retained; no diagnostic post-check repeats
    /// classifier selection from the annotation spelling.
    pub(super) fn retain_resolved_type_use_annotations(
        &mut self,
        scope: &super::CheckerScope<'_>,
        reference: &TypeRef,
    ) {
        let annotations = self
            .file
            .type_annotations
            .get(&reference.span.lo)
            .cloned()
            .unwrap_or_default();
        if annotations.iter().any(|annotation| {
            self.annotation_identity_in_scope(scope, annotation)
                == Some(crate::types::wk::unsafe_variance())
        }) {
            self.resolved_unsafe_variance_occurrences
                .insert((reference.span.lo, reference.span.hi));
        }
    }

    /// Bind the annotations on a complete source type occurrence in the declaration's lexical
    /// scope. Finalized signatures can supply the semantic type without recursively resolving its
    /// source tree, but variance checking still needs the already-bound `@UnsafeVariance` fact on
    /// any nested occurrence. This is the same identity lookup used by ordinary type resolution;
    /// it does not infer the annotation from its spelling.
    pub(super) fn retain_resolved_type_use_annotation_tree(
        &mut self,
        scope: &super::CheckerScope<'_>,
        reference: &TypeRef,
    ) {
        self.retain_resolved_type_use_annotations(scope, reference);
        let children = if super::function_type_ref_shape(reference).is_some() {
            reference
                .fun_params
                .iter()
                .chain(reference.arg.as_deref())
                .collect::<Vec<_>>()
        } else {
            reference.targs.iter().collect::<Vec<_>>()
        };
        for child in children {
            self.retain_resolved_type_use_annotation_tree(scope, child);
        }
    }

    /// Restore the same fact from the checked spelling that Pass 1 finalized for a declaration.
    pub(super) fn retain_finalized_type_use_annotations(
        &mut self,
        reference: &TypeRef,
        spelled: &crate::spelling::Spelled,
    ) {
        if spelled.annotations.iter().any(|annotation| {
            matches!(
                annotation,
                crate::spelling::TypeUseAnnotation::Checked(annotation)
                    if annotation.annotation == crate::types::wk::unsafe_variance()
            )
        }) {
            self.resolved_unsafe_variance_occurrences
                .insert((reference.span.lo, reference.span.hi));
        }
    }

    /// Publish the superclass occurrence from the classifier header Pass 1 already finalized.
    /// The detached parser copy is not resolved again: doing so would lose the declaration's
    /// lexical visibility policy, while editor navigation still needs the selected identity at
    /// the written source span.
    pub(super) fn record_finalized_superclass_type(&mut self, span: Span, semantic: Ty) {
        self.resolved_type_tys.insert((span.lo, span.hi), semantic);
    }

    /// Publish a finalized supertype occurrence and its bind-once alias provenance for semantic
    /// post-checks. Pass 2 must not resolve the reparsed header spelling again.
    pub(super) fn record_finalized_supertype(
        &mut self,
        reference: &TypeRef,
        semantic: Ty,
        spelled: &crate::spelling::Spelled,
    ) {
        self.resolved_type_tys
            .insert((reference.span.lo, reference.span.hi), semantic);
        self.retain_finalized_alias_applications(reference, spelled);
    }

    /// Rebind a reparsed value-parameter annotation to the semantic type finalized in Pass 1.
    /// Signatures store a vararg's callable-facing array type, while the annotation occurrence
    /// denotes its element type; record that syntax-facing view and return the callable-facing type
    /// used for the lexical parameter binding.
    pub(super) fn record_finalized_parameter_type(
        &mut self,
        reference: &TypeRef,
        is_vararg: bool,
        semantic: Ty,
        spelled: &crate::spelling::Spelled,
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
        self.retain_finalized_alias_applications(reference, spelled);
        self.check_finalized_type_reference_usage(reference, declaration);
        self.check_finalized_type_bounds(reference, declaration, spelled);
        semantic
    }

    /// Rebind an ordinary declaration annotation to the semantic type selected in Pass 1.
    /// `check_declaration_type` publishes both occurrence and declaration channels; the finalized
    /// path must preserve that same `TypeInfo` contract without repeating lookup.
    pub(super) fn record_finalized_declaration_type(
        &mut self,
        reference: &TypeRef,
        semantic: Ty,
        spelled: &crate::spelling::Spelled,
    ) -> Ty {
        self.resolved_type_tys
            .insert((reference.span.lo, reference.span.hi), semantic);
        self.resolved_declaration_types
            .insert((reference.span.lo, reference.span.hi), semantic);
        self.retain_finalized_alias_applications(reference, spelled);
        self.check_finalized_type_reference_usage(reference, semantic);
        self.check_finalized_type_bounds(reference, semantic, spelled);
        semantic
    }
}
