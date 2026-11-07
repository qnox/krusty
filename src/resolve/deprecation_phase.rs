//! kotlinc's `deprecationError` diagnostics: one report that is a warning while the language
//! feature deprecating the construct is off, and an error once the feature is on. The warning
//! carries kotlinc's `toDeprecationWarningMessage` suffix naming the language version and issue,
//! and `@Suppress` with the warning's name silences it; the error cannot be suppressed. A
//! `KtDiagnosticFactoryForDeprecation` names its warning `<NAME>_WARNING`; its error's name follows
//! no single rule (`<NAME>`, `<NAME>_ERROR`, or another diagnostic's), so each site names both.

use super::*;

/// kotlinc's names for the two halves of one `deprecationError` diagnostic.
#[derive(Clone, Copy)]
pub(super) struct DeprecationNames {
    pub(super) error: crate::diag::DiagnosticName,
    pub(super) warning: crate::diag::DiagnosticName,
}

impl Checker<'_> {
    /// Report `message` at `span` as kotlinc's `deprecationError` diagnostic, which the language
    /// selected policy (on when `forbidden`) turns into an error. `deprecation_message` is the
    /// exact warning text selected at the language-settings boundary; `None` means the selected
    /// release predates the split and reports a plain error. `names.warning` identifies the
    /// warning diagnostic for `@Suppress` and command-line warning policy.
    pub(super) fn report_deprecation_error(
        &mut self,
        span: Span,
        forbidden: bool,
        deprecation_message: Option<&str>,
        names: DeprecationNames,
        message: &str,
    ) {
        match deprecation_message.filter(|_| !forbidden) {
            Some(message) => {
                if self.suppresses_diagnostic(names.warning.as_str()) {
                    return;
                }
                self.diags
                    .warning_named(span, names.warning, message.to_string());
            }
            None => self
                .diags
                .error_named(span, names.error, message.to_string()),
        }
    }
}
