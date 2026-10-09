//! Syntax whose acceptance depends on a language feature the parser can see by itself.
//!
//! Each such use is parsed exactly as it is with the feature on, so later phases see the same
//! declarations either way. When the feature is off the use is retained in the file's
//! [`crate::ast::LanguageGates`] with kotlinc's `UNSUPPORTED_FEATURE` message, positioned where
//! kotlinc's checker reports it.

use super::Parser;
use crate::features::{FeatureGate, LangFeatures};

/// The parser's view of the features it gates.
#[derive(Default)]
pub(super) struct SyntaxGates {
    name_based_destructuring: FeatureGate,
}

impl SyntaxGates {
    pub(super) fn new(features: &LangFeatures) -> Self {
        Self {
            name_based_destructuring: features.gate("NameBasedDestructuring"),
        }
    }
}

impl Parser<'_> {
    /// `NameBasedDestructuring`: square-bracket and full-form destructuring.
    pub(super) fn name_based_destructuring(&self) -> bool {
        self.gates.name_based_destructuring.is_enabled()
    }

    /// The `[` of a square-bracket destructuring at the cursor. The syntax is parsed at every
    /// language level; it needs `NameBasedDestructuring`.
    pub(super) fn gate_bracket_destructuring(&mut self) {
        let span = self.tok().span;
        let gate = self.gates.name_based_destructuring.clone();
        self.file.language_gates.require(&gate, span);
    }
}
