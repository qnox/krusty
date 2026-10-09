//! Language-feature gates one parsed file carries: the uses of syntax its language settings do not
//! support, found while parsing.

use std::sync::Arc;

use crate::diag::Span;
use crate::features::FeatureGate;

/// One use of syntax whose language feature is disabled, with the error kotlinc reports for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedSyntax {
    pub span: Span,
    pub message: Arc<str>,
}

#[derive(Default)]
pub struct LanguageGates {
    /// Rejected syntax in parse order. The parser retains these rather than reporting them, so a
    /// gated use does not count as a syntax error that keeps the file's declarations out of the
    /// module; the frontend reports them once the file is admitted.
    pub unsupported_syntax: Vec<UnsupportedSyntax>,
}

impl LanguageGates {
    /// Retain a use of `gate`'s syntax at `span` when that feature is disabled.
    pub fn require(&mut self, gate: &FeatureGate, span: Span) {
        if let Some(message) = gate.unsupported_message() {
            self.unsupported_syntax.push(UnsupportedSyntax {
                span,
                message: message.clone(),
            });
        }
    }
}
