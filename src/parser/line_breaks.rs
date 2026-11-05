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

    /// Consume an initializer's `=` when the next non-newline token is one.
    ///
    /// Kotlin's property grammar is `… (NL* '=' NL* expression)?`, so a declaration may put its
    /// initializer on the following line:
    ///
    /// ```kotlin
    /// val a: Map<String, Int>
    ///     = mapOf("x" to 1)
    /// ```
    ///
    /// Stopping the declaration at the newline instead leaves the `=` to be read as the start of the
    /// next declaration or statement, which is not a recoverable position — nothing else in the
    /// grammar begins with `=`, so looking past the newlines cannot swallow anything but this
    /// property's own initializer.
    pub(super) fn eat_initializer_eq(&mut self) -> Option<Span> {
        self.skip_plain_newlines_before(TokenKind::Eq);
        self.eat_span(TokenKind::Eq)
    }

    /// Consume the colon that introduces a function or getter return type, or a secondary
    /// constructor's delegation call, with the line breaks Kotlin allows around it: `NL* ':' NL*`.
    /// As with an initializer's `=`, no declaration or statement begins with `:`, so looking past
    /// the line breaks cannot take anything but this declaration's own colon.
    pub(super) fn eat_declaration_colon(&mut self) -> bool {
        self.skip_plain_newlines_before(TokenKind::Colon);
        let colon = self.eat(TokenKind::Colon);
        if colon {
            self.skip_plain_newlines();
        }
        colon
    }

    /// Consume the `.` that ends a declaration's receiver type (or one of its qualifier segments),
    /// with the line breaks Kotlin allows on either side of it: `receiverType NL* '.' NL*`.
    pub(super) fn eat_receiver_dot(&mut self) -> bool {
        self.skip_plain_newlines_before(TokenKind::Dot);
        let dot = self.eat(TokenKind::Dot);
        if dot {
            self.skip_plain_newlines();
        }
        dot
    }

    /// [`Self::eat_receiver_dot`], reporting a missing `.`.
    pub(super) fn expect_receiver_dot(&mut self) {
        if !self.eat_receiver_dot() {
            self.diags.error(self.tok().span, "expected '.'");
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
