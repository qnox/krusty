//! kotlinc's `FirStandaloneQualifierChecker` for an object qualifier written with type arguments
//! and used as a value (`val u = Unit<Any>`, `val p by MyUnitT<Any>`).
//!
//! The ordinary classifier tower supplies the selected classifier and exact alias provenance.
//! This check consumes that decision together with retained source syntax; it never resolves the
//! qualifier spelling a second time.

use super::*;

const MESSAGE: &str = "object access cannot have explicit type arguments.";

impl Checker<'_> {
    /// Report a standalone singleton qualifier after the classifier tower has committed its
    /// binding. `through_alias` belongs to that same selected binding, not to a spelling probe.
    pub(super) fn check_standalone_qualifier_type_arguments(
        &mut self,
        expression: ExprId,
        classifier: TypeName,
        through_alias: bool,
    ) {
        if self.discover_anonymous_captures
            || !self
                .file
                .standalone_type_argument_qualifiers
                .contains(&expression.0)
        {
            return;
        }
        let Some(arguments) = self.file.call_type_args.get(&expression.0) else {
            return;
        };
        let has_companion = self
            .resolver()
            .classifier(classifier)
            .is_some_and(|declaration| declaration.companion_object.is_some());
        let accepted = !has_companion && (through_alias || classifier == crate::types::wk::unit());
        if accepted {
            self.report_deprecation_error(
                arguments.span,
                self.file.forbid_useless_type_arguments,
                self.file
                    .explicit_type_arguments_in_property_access_warning
                    .as_deref(),
                crate::diag::names::EXPLICIT_TYPE_ARGUMENTS_IN_PROPERTY_ACCESS_WARNING,
                MESSAGE,
            );
        } else {
            self.diags.error(arguments.span, MESSAGE.to_string());
        }
    }
}
