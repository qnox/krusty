//! Ignorability annotations while the return value checker is off: kotlinc's
//! `FirReturnValueAnnotationsChecker`.
//!
//! `@MustUseReturnValues` and `@IgnorableReturnValue` only mean something to the checker, so a
//! release that has not made the checker stable rejects them until `-Xreturn-value-checker`
//! enables it. The application is identified by its resolved annotation class, never by the
//! spelling it was written with.

use super::*;

impl Checker<'_> {
    /// Report `annotation`, an application of `internal`, when it is an ignorability annotation
    /// the language settings do not admit.
    pub(super) fn check_ignorability_annotation(
        &mut self,
        annotation: &AnnotationRef,
        internal: TypeName,
    ) {
        let language = &self.file;
        if language.return_value_checker_is_stable
            || language.return_value_checker != crate::features::ReturnValueCheckerMode::Disabled
        {
            return;
        }
        if internal != crate::types::wk::must_use_return_values()
            && internal != crate::types::wk::ignorable_return_value()
        {
            return;
        }
        if self.suppresses_diagnostic("IGNORABILITY_ANNOTATIONS_WITH_CHECKER_DISABLED") {
            return;
        }
        self.diags.error_named(
            Span::new(annotation.at, annotation.span.hi),
            crate::diag::names::IGNORABILITY_ANNOTATIONS_WITH_CHECKER_DISABLED,
            "Ignorability-related annotations are experimental and cannot be used with \
             -Xreturn-value-checker in disabled state."
                .to_string(),
        );
    }
}
