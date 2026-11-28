//! The diagnostic for a classifier the current site may not name: one that is not visible here,
//! or one in a package its module does not export.

use super::*;

impl Checker<'_> {
    pub(super) fn inaccessible_classifier(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> Option<(TypeName, crate::symbol_resolver::ClassifierDenial)> {
        let InheritedNestedClassifier::Found(internal) = self.select_classifier(scope, name) else {
            return None;
        };
        self.inaccessible_classifier_reference(internal)
            .map(|denial| (internal, denial))
    }

    pub(super) fn reject_inaccessible_classifier_expression(
        &mut self,
        expression: ExprId,
        internal: TypeName,
    ) -> bool {
        let Some(denial) = self.inaccessible_classifier_reference(internal) else {
            return false;
        };
        if self.suppresses_diagnostic(denial.suppression_name()) {
            return false;
        }
        crate::trace_compiler!(
            "resolve",
            "reject inaccessible classifier expression={expression:?} internal={} access={denial:?} ast={:?}",
            internal,
            self.file.expr(expression),
        );
        let reference = self.span(expression);
        let display = match self.file.expr(expression) {
            Expr::Name(name) => name.clone(),
            _ => internal.render().replace(['/', '$'], "."),
        };
        let message = if denial
            == crate::symbol_resolver::ClassifierDenial::Access(
                crate::libraries::ClassifierAccess::PackagePrivate,
            ) {
            self.classifier_access_display(internal).map_or_else(
                || inaccessible_classifier_message(&display, &denial),
                |display| format!("cannot access '{display}': it is package-private in file."),
            )
        } else {
            inaccessible_classifier_message(&display, &denial)
        };
        self.diags.error_with_identity(
            reference,
            DiagnosticIdentity::ClassifierAccess {
                reference,
                classifier: internal,
            },
            message,
        );
        // The receiver is semantically bound but unusable because that classifier access was
        // diagnosed. Preserve that provenance across cached expression reads so an enclosing
        // member access does not add an unrelated "Unresolved reference" cascade.
        self.silent_error_exprs.insert(expression);
        true
    }
}
