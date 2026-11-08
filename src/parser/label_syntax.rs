//! Where a label is written, which is whitespace-sensitive in Kotlin.
//!
//! A label DEFINITION is a name immediately followed by `@` (`outer@ for (…)`, `run loop@{ … }`):
//! kotlinc requires the `@` to touch the name, so `value @Ann` and a getter body ending in a name
//! followed by an annotation on the next line are not labels. A label REFERENCE (`return@l`,
//! `break@l`, `this@Outer`) takes an `@` on the same line as its keyword.

use super::*;

impl Parser<'_> {
    /// Whether the token at `index` is a name immediately followed by `@`, defining a label.
    pub(super) fn at_label_definition_at(&self, index: usize) -> bool {
        let (Some(name), Some(at)) = (self.t.get(index), self.t.get(index + 1)) else {
            return false;
        };
        name.kind == TokenKind::Ident
            && at.kind == TokenKind::At
            && self.syntactic_ident_span(*name).hi == at.span.lo
    }

    /// Whether the current `@` continues a label reference on the line of its keyword.
    pub(super) fn at_label_reference(&self) -> bool {
        self.at(TokenKind::At) && !self.newline_before()
    }
}
