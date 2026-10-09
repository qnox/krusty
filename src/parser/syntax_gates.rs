//! Syntax whose acceptance depends on a language feature the parser can see by itself.
//!
//! Each such use is parsed exactly as it is with the feature on, so later phases see the same
//! declarations either way. When the feature is off the use is retained in the file's
//! [`crate::ast::LanguageGates`] with kotlinc's `UNSUPPORTED_FEATURE` message, positioned where
//! kotlinc's checker reports it.

use super::{Parser, TokenKind};
use crate::diag::Span;
use crate::features::{FeatureGate, LangFeatures};

/// The parser's view of the features it gates.
#[derive(Default)]
pub(super) struct SyntaxGates {
    name_based_destructuring: FeatureGate,
    unnamed_local_variables: FeatureGate,
}

impl SyntaxGates {
    pub(super) fn new(features: &LangFeatures) -> Self {
        Self {
            name_based_destructuring: features.gate("NameBasedDestructuring"),
            unnamed_local_variables: features.gate("UnnamedLocalVariables"),
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

    /// A local property named by the unescaped `_` token at the cursor: kotlinc's unnamed local
    /// variable, which needs `UnnamedLocalVariables` everywhere except in a destructuring entry and
    /// a `catch` parameter. Reported at the name. A `var` additionally has no name to write to,
    /// whatever the feature, which kotlinc reports at the `var` keyword.
    pub(super) fn gate_unnamed_local(&mut self, var_keyword: Option<Span>) {
        if !self.at_unnamed_local_name() {
            return;
        }
        if let Some(var_keyword) = var_keyword {
            self.file
                .language_gates
                .reject(var_keyword, "'var' properties require a name.");
        }
        let span = self.tok().span;
        let gate = self.gates.unnamed_local_variables.clone();
        self.file.language_gates.require(&gate, span);
    }

    /// Whether the cursor is the `_` token naming a local, as opposed to `` `_` ``, which names a
    /// variable called `_`.
    pub(super) fn at_unnamed_local_name(&self) -> bool {
        self.at(TokenKind::Ident) && self.text() == "_" && !self.escaped_ident()
    }
}
