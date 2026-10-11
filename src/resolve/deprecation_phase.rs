//! kotlinc's `deprecationError` diagnostics: one report that is a warning while the language
//! feature deprecating the construct is off, and an error once the feature is on. The warning
//! carries kotlinc's `toDeprecationWarningMessage` suffix naming the language version and issue,
//! and `@Suppress` with the warning's name silences it; the error cannot be suppressed. A
//! `KtDiagnosticFactoryForDeprecation` names its warning `<NAME>_WARNING`.

use super::*;

impl Checker<'_> {
    /// Report `message` at `span` as kotlinc's `deprecationError` diagnostic, which the language
    /// feature `feature` (on when `forbidden`) turns into an error. `warning` names the warning
    /// diagnostic for `@Suppress`.
    pub(super) fn report_deprecation_error(
        &mut self,
        span: Span,
        feature: &str,
        forbidden: bool,
        warning: &str,
        message: &str,
    ) {
        if forbidden {
            self.diags.error(span, message.to_string());
        } else if !self.suppresses_diagnostic(warning) {
            self.diags.warning(
                span,
                crate::features::deprecation_warning_message(feature, message),
            );
        }
    }
}
