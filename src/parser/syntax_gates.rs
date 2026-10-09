//! Syntax whose acceptance depends on a language feature the parser can see by itself.
//!
//! Each such use is parsed exactly as it is with the feature on, so later phases see the same
//! declarations either way. When the feature is off the use is retained in the file's
//! [`crate::ast::LanguageGates`] with kotlinc's `UNSUPPORTED_FEATURE` message, positioned where
//! kotlinc's checker reports it.

use super::{Parser, TokenKind};
use crate::diag::Span;
use crate::features::{FeatureGate, LangFeatures};

/// The parser's view of the features it gates, and whether it is inside a body: a statement or an
/// expression, where every declaration is local.
#[derive(Default)]
pub(super) struct SyntaxGates {
    name_based_destructuring: FeatureGate,
    unnamed_local_variables: FeatureGate,
    local_type_aliases: FeatureGate,
    /// How many statements and expressions enclose the cursor.
    body_depth: u32,
}

impl SyntaxGates {
    pub(super) fn new(features: &LangFeatures) -> Self {
        Self {
            name_based_destructuring: features.gate("NameBasedDestructuring"),
            unnamed_local_variables: features.gate("UnnamedLocalVariables"),
            local_type_aliases: features.gate("LocalTypeAliases"),
            body_depth: 0,
        }
    }

    pub(super) fn enter_body(&mut self) {
        self.body_depth += 1;
    }

    pub(super) fn leave_body(&mut self) {
        self.body_depth -= 1;
    }

    /// A declaration starting here is local: kotlinc's `isLocal`, which holds for everything
    /// declared in a body, including the members of local and anonymous classes.
    fn in_body(&self) -> bool {
        self.body_depth > 0
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

    /// A type alias declared at `start` (its annotations and modifiers included). Outside a body it
    /// is top-level or nested in a classifier, which `NestedTypeAliases` governs; in a body it is
    /// local and needs `LocalTypeAliases`.
    pub(super) fn gate_type_alias(&mut self, start: Span) {
        if self.gates.in_body() {
            let gate = self.gates.local_type_aliases.clone();
            self.file.language_gates.require(&gate, start);
        }
    }
}
