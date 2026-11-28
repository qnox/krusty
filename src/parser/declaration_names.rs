//! The name a declaration introduces.
//!
//! A name written as underscores only (`_`, `__`) and without back-ticks is reserved. The parser
//! records that syntax fact on the name's position; the checker decides which declarations kotlinc
//! rejects with it, since a lambda's or destructuring's bare `_` is an unused binding instead.

use super::*;

impl Parser<'_> {
    pub(super) fn ident_or_error(&mut self, what: &str) -> String {
        self.declaration_name_span = self.tok().span;
        if self.at(TokenKind::Ident) {
            let n = self.text().to_string();
            if n.bytes().all(|byte| byte == b'_') && !self.escaped_ident() {
                self.file
                    .unescaped_underscore_declarations
                    .insert(self.declaration_name_span.lo);
            }
            self.bump();
            n
        } else {
            self.diags
                .error(self.tok().span, format!("expected {what}"));
            "<error>".to_string()
        }
    }
}
