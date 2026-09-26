//! Checking one annotation APPLICATION — its arguments, against the annotation class it names.
//!
//! The applications themselves were resolved and validated during header collection. What is left
//! here is the argument expressions, and whether this pass can still see them at all: a RESTRICTED
//! pass re-enters a declaration it did not select purely to rebuild that declaration's scopes, and
//! the annotation syntax of the members it skipped has already been released. Which passes those
//! are is an explicit declaration, not something inferred from the data a pass happens to carry —
//! see [`super::source_fragment`].

use super::*;

impl Checker<'_> {
    pub(super) fn check_annotation_application(
        &mut self,
        scope: &CheckerScope<'_>,
        annotation: &AnnotationRef,
        arguments: &[ExprId],
    ) {
        // Resolve the application in its owning lexical scope. Pass 1's declaration-header
        // inventory and Pass 2's body checking use the same scope rules.
        let reference = TypeRef {
            name: annotation.name.clone(),
            flags: TrFlags::default(),
            arg: None,
            targs: Vec::new(),
            span: annotation.span,
            fun_params: Vec::new(),
            fun_context_count: 0,
        };
        let ty = self.type_ref_ty_reported(scope, &reference);
        let Some(internal) = ty.kotlin_class_internal() else {
            return;
        };
        self.check_bound_annotation_application(scope, annotation, arguments, internal);
    }

    /// Check an annotation occurrence whose classifier was selected during declaration-header
    /// resolution. Metadata publication calls this with the stable occurrence binding; it must not
    /// repeat classifier lookup after actualization has removed target-inapplicable declarations.
    pub(super) fn check_bound_annotation_application(
        &mut self,
        scope: &CheckerScope<'_>,
        annotation: &AnnotationRef,
        arguments: &[ExprId],
        internal: TypeName,
    ) {
        if arguments
            .iter()
            .any(|argument| self.file.expr_span(*argument).is_none())
        {
            // A restricted body pass re-enters declarations solely to recreate lexical scopes;
            // annotation syntax on skipped neighbors may already be gone. The focused metadata
            // publisher never calls this method without its explicitly retained argument fragment,
            // while a complete pass must still fail closed.
            assert!(
                self.fragment.may_observe_released_annotation_syntax(),
                "a complete pass reached released annotation syntax; fragment={:?}",
                self.fragment
            );
            return;
        }
        if !self.file.is_common
            && self.is_optional_expectation_classifier(internal)
            && !self.suppresses_diagnostic("OPTIONAL_DECLARATION_USAGE_IN_NON_COMMON_SOURCE")
        {
            self.diags.error(
                annotation.span,
                format!("unresolved reference '{}'.", annotation.name),
            );
            return;
        }
        let Some(shape) = self.annotation_shape(internal) else {
            self.diags.error(
                annotation.span,
                "resolved annotation has no semantic element declaration".to_string(),
            );
            return;
        };
        if !self.check_annotation_arguments(scope, annotation.span, &shape, arguments, None) {
            return;
        }
        let Some(applied) = self.fold_annotation_application(internal, arguments) else {
            self.diags.error(
                annotation.span,
                "annotation argument is not a supported compile-time constant".to_string(),
            );
            return;
        };
        self.applied_annotations
            .insert((annotation.span.lo, annotation.span.hi), applied);
    }
}
