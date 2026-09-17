//! Declaration-body lookahead that preserves an absent declaration's exact token span.

use super::Parser;
use crate::token::TokenKind;

impl Parser<'_> {
    /// Consume a declaration body's opening brace across line breaks. When no body follows, restore
    /// the cursor so those line breaks and any intervening trivia belong to the following syntax,
    /// not to the bodyless declaration's span.
    pub(super) fn eat_optional_declaration_body_open(&mut self) -> bool {
        let before_line_breaks = self.i;
        self.skip_newlines();
        if self.eat(TokenKind::LBrace) {
            true
        } else {
            self.i = before_line_breaks;
            false
        }
    }
}
