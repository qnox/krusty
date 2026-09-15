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
        if arguments
            .iter()
            .any(|argument| self.file.expr_span(*argument).is_none())
        {
            // Annotation applications were resolved and validated during header collection. A
            // RESTRICTED pass re-enters a declaration it did not select solely to recreate that
            // declaration's lexical type and value scopes, and the annotation expressions of the
            // members it skipped are outside the fragment it retains. Two passes are restricted
            // this way: Pass-1 default checking, and inline preparation, which selects only the
            // inline declarations it must expand. An UNRESTRICTED pass has every expression and
            // must never reach released syntax.
            assert!(
                self.fragment.may_observe_released_annotation_syntax(),
                "a complete pass reached released annotation syntax; fragment={:?}",
                self.fragment
            );
            return;
        }
        let legacy_prebound = self
            .resolved_index
            .is_none()
            .then(|| {
                super::annotation_legacy_bridge::resolved_annotation(
                    &self.module,
                    self.file_index,
                    annotation,
                )
            })
            .flatten();
        let internal = if let Some(internal) = legacy_prebound {
            internal
        } else {
            // Pass 1 binds declaration-header annotations only. A local declaration first appears
            // while its active Pass-2 body is checked, where the real lexical type scope is finally
            // available; resolve it here instead of requiring a whole-body annotation inventory
            // traversal during signature collection.
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
            internal
        };
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
