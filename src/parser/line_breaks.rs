//! Line-break handling shared by the grammar.
//!
//! The lexer represents a physical line break and an explicit `;` both as `Newline`. Kotlin's
//! grammar allows `NL*` in specific positions and treats a break before `||`, `&&`, or `?:` as a
//! continuation; these operations keep every caller on the same rules.
//!
//! Inside `(…)` and `[…]` a line break is not a terminator at all: kotlinc's parser disables
//! newlines there and re-enables them in every `{…}` body nested inside (lambda, block, `when`
//! body, object literal). [`Parser::with_newlines_insignificant`] and
//! [`Parser::with_newlines_significant`] keep that mode, and `bump` honours it, so `(a\n+ b)` and
//! `f(x\nor y)` continue the expression exactly as kotlinc does.

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
        self.skip_insignificant_newlines();
        let result = parse(self);
        self.newlines_insignificant = outer;
        self.skip_insignificant_newlines();
        result
    }

    /// While line breaks are insignificant, the cursor never rests on one. An explicit `;` is kept:
    /// it is a token, not whitespace.
    pub(super) fn skip_insignificant_newlines(&mut self) {
        if self.newlines_insignificant {
            self.skip_plain_newlines();
        }
    }

    pub(super) fn skip_newlines(&mut self) {
        while self.at(TokenKind::Newline) {
            self.bump();
        }
    }

    /// Return the first token after physical line breaks at `index`, stopping at an explicit `;`.
    ///
    /// The lexer deliberately represents both forms as `Newline`, so grammar lookahead must use this
    /// helper instead of open-coding a token-kind loop whenever a construct permits `NL*` but not a
    /// semicolon. Keeping that distinction here prevents declaration callers from gradually acquiring
    /// different continuation rules.
    pub(super) fn after_plain_newlines(&self, mut index: usize) -> usize {
        while self
            .t
            .get(index)
            .is_some_and(|token| token.kind == TokenKind::Newline && token.text(self.src) != ";")
        {
            index += 1;
        }
        index
    }

    /// Advance over physical line breaks while preserving an explicit semicolon as a declaration
    /// boundary. Mutating callers share the same lexer distinction as lookahead callers through
    /// `after_plain_newlines`; there must not be a second newline-scanning implementation here.
    pub(super) fn skip_plain_newlines(&mut self) {
        self.i = self.after_plain_newlines(self.i);
    }

    /// Consume physical line breaks only when `expected` follows them. A failed lookahead leaves the
    /// parser at the original newline so the enclosing declaration can terminate normally.
    pub(super) fn skip_plain_newlines_before(&mut self, expected: TokenKind) {
        let next = self.after_plain_newlines(self.i);
        if self.t.get(next).is_some_and(|token| token.kind == expected) {
            self.i = next;
        }
    }

    /// Kotlin treats a newline before `||`, `&&`, or `?:` as a line CONTINUATION, not a statement
    /// terminator (`cond\n  && other`, `x\n  ?: default`): the operator cannot begin a statement, so
    /// the expression keeps going. Peek past the newline(s); if such an operator follows, consume them
    /// so the binary/elvis loop sees the operator on the same logical line. (`+`/`-`/`*` deliberately do
    /// NOT continue — kotlinc parses a leading `-x` as a fresh unary-prefix statement, per the grammar
    /// which allows `NL*` before `||`/`&&`/elvis but not before the additive/multiplicative operators.)
    pub(super) fn skip_newlines_before_continuation_op(&mut self) {
        if !self.at(TokenKind::Newline) {
            return;
        }
        let mut j = self.i;
        while self.t.get(j).is_some_and(|t| t.kind == TokenKind::Newline) {
            j += 1;
        }
        let continues = match self.t.get(j).map(|t| t.kind) {
            Some(TokenKind::OrOr | TokenKind::AndAnd) => true,
            Some(TokenKind::Question) => self
                .t
                .get(j + 1)
                .is_some_and(|t| t.kind == TokenKind::Colon),
            _ => false,
        };
        if continues {
            self.skip_newlines();
        }
    }
}
