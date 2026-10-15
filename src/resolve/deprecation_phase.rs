//! kotlinc's `deprecationError` diagnostics: one report that is a warning while the language
//! feature deprecating the construct is off, and an error once the feature is on. The warning
//! carries kotlinc's `toDeprecationWarningMessage` suffix naming the language version and issue,
//! and `@Suppress` with the warning's name silences it; the error cannot be suppressed. A
//! `KtDiagnosticFactoryForDeprecation` names its warning `<NAME>_WARNING`.

use super::*;

impl Checker<'_> {
    /// Report `message` at `span` as kotlinc's `deprecationError` diagnostic, which the language
    /// selected policy (on when `forbidden`) turns into an error. `deprecation_message` is the
    /// exact warning text selected at the language-settings boundary; `None` means the selected
    /// release predates the split and reports a plain error. `warning` identifies the warning
    /// diagnostic for `@Suppress` and command-line warning policy.
    pub(super) fn report_deprecation_error(
        &mut self,
        span: Span,
        forbidden: bool,
        deprecation_message: Option<&str>,
        warning: crate::diag::CompilerWarningName,
        message: &str,
    ) {
        if forbidden {
            self.diags.error(span, message.to_string());
        } else if let Some(message) = deprecation_message {
            if self.suppresses_diagnostic(warning.as_str()) {
                return;
            }
            self.diags.warning_named(span, warning, message.to_string());
        } else {
            self.diags.error(span, message.to_string());
        }
    }
}
