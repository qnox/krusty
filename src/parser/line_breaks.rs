//! Line breaks in the grammar.
//!
//! Kotlin treats a line break as whitespace except at a few grammar points, kotlinc's
//! `newlineBeforeCurrentToken` checks. The parser therefore reads tokens without physical line
//! breaks and records, per token, whether one preceded it; an explicit `;` stays a token. Only the
//! points below consult that record:
//!
//! * an expression is interrupted by a line break unless the next token continues it
//!   ([`Parser::interrupted_with_newline`]): binary operators, postfix operations, calls, index
//!   brackets, and trailing lambdas do not cross a line;
//! * a statement or optional trailing element may end at a line break
//!   ([`Parser::at_eol_or_semicolon`]), as `return` does before a value on the next line;
//! * a call suffix after a selector, and a label reference, stay on their line.
//!
//! Inside `(…)` and `[…]` a line break is not significant at all: kotlinc disables newlines there
//! and re-enables them in every `{…}` body nested inside (lambda, block, `when` body, object
//! literal). [`Parser::with_newlines_insignificant`] and [`Parser::with_newlines_significant`]
//! keep that mode, so `(a\n+ b)` and `f(x\nor y)` continue the expression exactly as kotlinc does.

use super::*;

impl Parser<'_> {
    /// Parse a parenthesized or bracketed construct, in which physical line breaks are whitespace.
    pub(super) fn with_newlines_insignificant<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.with_newline_mode(true, parse)
    }

    /// Parse a `{…}` body, which restores statement-terminating line breaks inside any enclosing
    /// parentheses.
    pub(super) fn with_newlines_significant<T>(&mut self, parse: impl FnOnce(&mut Self) -> T) -> T {
        self.with_newline_mode(false, parse)
    }

    fn with_newline_mode<T>(
        &mut self,
        insignificant: bool,
        parse: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let outer = std::mem::replace(&mut self.newlines_insignificant, insignificant);
        let result = parse(self);
        self.newlines_insignificant = outer;
        result
    }

    /// Whether a physical line break precedes the current token where line breaks are significant:
    /// outside `(…)`/`[…]`, or in a `{…}` body nested inside them. kotlinc's
    /// `newlineBeforeCurrentToken`.
    pub(super) fn newline_before(&self) -> bool {
        self.newline_before_at(self.i)
    }

    /// [`Self::newline_before`] for the token at `index`.
    pub(super) fn newline_before_at(&self, index: usize) -> bool {
        !self.newlines_insignificant && self.newline_before.get(index).copied().unwrap_or(true)
    }

    /// Whether a line break before the current token ends the expression being parsed: kotlinc's
    /// `interruptedWithNewLine`. Only member access (`.`, `?.`), a cast (`as`, `as?`), `:`, elvis
    /// (`?:`), `&&`, and `||` continue an expression across a line break; any other operator, call
    /// parenthesis, index bracket, or trailing lambda on the next line starts a new statement.
    pub(super) fn interrupted_with_newline(&self) -> bool {
        self.newline_before() && !self.at_newline_continuation()
    }

    fn at_newline_continuation(&self) -> bool {
        match self.kind() {
            TokenKind::Dot | TokenKind::Colon | TokenKind::AndAnd | TokenKind::OrOr => true,
            TokenKind::Question => self
                .t
                .get(self.i + 1)
                .is_some_and(|token| matches!(token.kind, TokenKind::Dot | TokenKind::Colon)),
            TokenKind::Ident => self.keyword_text("as"),
            _ => false,
        }
    }

    /// kotlinc's `EOL_OR_SEMICOLON`: an explicit `;`, a line break before the current token, or the
    /// end of input. This is where a statement or an optional trailing element may end.
    pub(super) fn at_eol_or_semicolon(&self) -> bool {
        self.at(TokenKind::Semicolon) || self.at(TokenKind::Eof) || self.newline_before()
    }

    /// An empty span where the previous token ends: the position of syntax the source leaves
    /// implicit (a deferred local's default value), which belongs to the line it is implied on
    /// rather than to whatever token starts the next one.
    pub(super) fn implied_span(&self) -> Span {
        let end = self
            .i
            .checked_sub(1)
            .map_or(self.tok().span.lo, |previous| self.t[previous].span.hi);
        Span::new(end, end)
    }

    /// Skip explicit `;` separators. A line break is not a token here; see the module docs.
    pub(super) fn skip_semicolons(&mut self) {
        while self.at(TokenKind::Semicolon) {
            self.bump();
        }
    }
}

/// The lexer's tokens without physical line breaks, with whether one preceded each kept token.
/// An explicit `;`, which the lexer spells `Newline`, is kept as `Semicolon`: it is a token, not
/// whitespace.
pub(super) fn grammar_tokens(src: &str, tokens: &[Token]) -> (Vec<Token>, Vec<bool>) {
    let mut kept = Vec::with_capacity(tokens.len());
    let mut newline_before = Vec::with_capacity(tokens.len());
    let mut pending_newline = false;
    for token in tokens {
        let mut token = *token;
        if token.kind == TokenKind::Newline {
            if token.text(src) != ";" {
                pending_newline = true;
                continue;
            }
            token.kind = TokenKind::Semicolon;
        }
        newline_before.push(pending_newline || token.kind == TokenKind::Eof);
        kept.push(token);
        pending_newline = false;
    }
    (kept, newline_before)
}
