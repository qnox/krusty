//! Stage A: byte slice → token stream.
//!
//! Newlines are emitted as `Newline` tokens (Kotlin uses them as statement separators); other
//! whitespace and comments are skipped. The lexer never fails — unknown bytes become
//! `TokenKind::Unknown` with a diagnostic, so later stages can still make progress.

use crate::diag::{DiagSink, Span};
use crate::token::{keyword, Token, TokenKind};
use unicode_general_category::{get_general_category, GeneralCategory};

pub fn lex(src: &str, diags: &mut DiagSink) -> Vec<Token> {
    lexer(src, diags).run()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NameTokenKind {
    Ident,
    Package,
    Import,
    At,
    Dot,
    Operator,
    Newline,
}

#[derive(Clone, Copy, Debug)]
pub struct NameToken {
    pub kind: NameTokenKind,
    pub span: Span,
}

impl NameToken {
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.span.lo as usize..self.span.hi as usize]
    }
}

/// Lex only the tokens needed by semantic analysis (identifiers, namespace/annotation separators,
/// and operator spellings). This follows the exact ordinary lexer path, including templates and
/// backtick names, while avoiding a second full-token allocation for editor symbol indexing.
pub fn lex_name_tokens(src: &str, diags: &mut DiagSink) -> Vec<NameToken> {
    lexer(src, diags).run_names()
}

fn lexer<'a>(src: &'a str, diags: &'a mut DiagSink) -> Lexer<'a> {
    Lexer {
        b: src.as_bytes(),
        i: 0,
        out: Vec::new(),
        diags,
        pending: std::collections::VecDeque::new(),
    }
}

struct Lexer<'a> {
    b: &'a [u8],
    i: usize,
    out: Vec<Token>,
    diags: &'a mut DiagSink,
    /// Tokens produced ahead of time (string-template expansion), drained before lexing more.
    pending: std::collections::VecDeque<Token>,
}

impl<'a> Lexer<'a> {
    fn run(mut self) -> Vec<Token> {
        loop {
            let tok = self.next_token();
            let is_eof = tok.kind == TokenKind::Eof;
            self.out.push(tok);
            if is_eof {
                break;
            }
        }
        self.out
    }

    fn run_names(mut self) -> Vec<NameToken> {
        let mut names = Vec::new();
        loop {
            let token = self.next_token();
            let kind = match token.kind {
                TokenKind::Ident => Some(NameTokenKind::Ident),
                TokenKind::KwPackage => Some(NameTokenKind::Package),
                TokenKind::KwImport => Some(NameTokenKind::Import),
                TokenKind::At => Some(NameTokenKind::At),
                TokenKind::Dot => Some(NameTokenKind::Dot),
                TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent
                | TokenKind::EqEq
                | TokenKind::NotEq
                | TokenKind::RefEq
                | TokenKind::RefNe
                | TokenKind::Lt
                | TokenKind::LtEq
                | TokenKind::Gt
                | TokenKind::GtEq
                | TokenKind::AndAnd
                | TokenKind::OrOr
                | TokenKind::Not
                | TokenKind::DotDot
                | TokenKind::DotDotLt
                | TokenKind::PlusPlus
                | TokenKind::MinusMinus
                | TokenKind::PlusEq
                | TokenKind::MinusEq
                | TokenKind::StarEq
                | TokenKind::SlashEq
                | TokenKind::PercentEq => Some(NameTokenKind::Operator),
                TokenKind::Newline => Some(NameTokenKind::Newline),
                _ => None,
            };
            if let Some(kind) = kind {
                names.push(NameToken {
                    kind,
                    span: token.span,
                });
            }
            if token.kind == TokenKind::Eof {
                break;
            }
        }
        names
    }

    /// Return a queued token if any, else lex one fresh.
    fn next_token(&mut self) -> Token {
        if let Some(t) = self.pending.pop_front() {
            return t;
        }
        self.lex_one()
    }

    fn peek(&self) -> u8 {
        if self.i < self.b.len() {
            self.b[self.i]
        } else {
            0
        }
    }
    fn peek2(&self) -> u8 {
        if self.i + 1 < self.b.len() {
            self.b[self.i + 1]
        } else {
            0
        }
    }
    fn peek3(&self) -> u8 {
        if self.i + 2 < self.b.len() {
            self.b[self.i + 2]
        } else {
            0
        }
    }
    fn three(&mut self, kind: TokenKind) -> TokenKind {
        self.i += 3;
        kind
    }

    fn lex_one(&mut self) -> Token {
        self.skip_trivia();
        let lo = self.i as u32;
        if self.i >= self.b.len() {
            return Token {
                kind: TokenKind::Eof,
                span: Span::new(lo, lo),
            };
        }
        let c = self.b[self.i];
        if c == b'$' {
            if let Some(dollars) = self.multi_dollar_prefix_len() {
                return self.prefixed_string(lo, dollars);
            }
        }
        let kind = match c {
            b'\n' => {
                self.i += 1;
                TokenKind::Newline
            }
            b'(' => self.one(TokenKind::LParen),
            b')' => self.one(TokenKind::RParen),
            b'{' => self.one(TokenKind::LBrace),
            b'}' => self.one(TokenKind::RBrace),
            b'[' => self.one(TokenKind::LBracket),
            b']' => self.one(TokenKind::RBracket),
            b',' => self.one(TokenKind::Comma),
            b';' => self.one(TokenKind::Newline), // `;` is a statement/arm separator like a newline
            b':' if self.peek2() == b':' => self.two(TokenKind::ColonColon),
            b':' => self.one(TokenKind::Colon),
            b'.' if self.peek2() == b'.' && self.peek3() == b'<' => self.three(TokenKind::DotDotLt),
            b'.' if self.peek2() == b'.' => self.two(TokenKind::DotDot),
            b'.' if !self.peek2().is_ascii_digit() => self.one(TokenKind::Dot),
            b'+' if self.peek2() == b'+' => self.two(TokenKind::PlusPlus),
            b'+' if self.peek2() == b'=' => self.two(TokenKind::PlusEq),
            b'+' => self.one(TokenKind::Plus),
            b'-' if self.peek2() == b'>' => self.two(TokenKind::Arrow),
            b'-' if self.peek2() == b'-' => self.two(TokenKind::MinusMinus),
            b'-' if self.peek2() == b'=' => self.two(TokenKind::MinusEq),
            b'-' => self.one(TokenKind::Minus),
            b'*' if self.peek2() == b'=' => self.two(TokenKind::StarEq),
            b'*' => self.one(TokenKind::Star),
            b'/' if self.peek2() == b'=' => self.two(TokenKind::SlashEq),
            b'/' => self.one(TokenKind::Slash),
            b'%' if self.peek2() == b'=' => self.two(TokenKind::PercentEq),
            b'%' => self.one(TokenKind::Percent),
            b'=' if self.peek2() == b'=' && self.peek3() == b'=' => self.three(TokenKind::RefEq),
            b'=' if self.peek2() == b'=' => self.two(TokenKind::EqEq),
            b'=' => self.one(TokenKind::Eq),
            b'!' if self.peek2() == b'=' && self.peek3() == b'=' => self.three(TokenKind::RefNe),
            b'!' if self.peek2() == b'=' => self.two(TokenKind::NotEq),
            b'!' => self.one(TokenKind::Not), // `!!` (not-null) is two `Not`s in postfix position
            b'?' => self.one(TokenKind::Question),
            b'@' => self.one(TokenKind::At),
            b'<' if self.peek2() == b'=' => self.two(TokenKind::LtEq),
            b'<' => self.one(TokenKind::Lt),
            b'>' if self.peek2() == b'=' => self.two(TokenKind::GtEq),
            b'>' => self.one(TokenKind::Gt),
            b'&' if self.peek2() == b'&' => self.two(TokenKind::AndAnd),
            b'&' => self.one(TokenKind::Amp),
            b'|' if self.peek2() == b'|' => self.two(TokenKind::OrOr),
            b'"' => return self.string(lo),
            b'\'' => return self.char_lit(lo),
            b'`' => return self.backtick_ident(),
            b'0'..=b'9' => return self.number(lo),
            b'.' => return self.number(lo), // .5
            c if is_ascii_ident_start(c) || (c >= 0x80 && is_ident_start_at(self.b, self.i)) => {
                return self.ident(lo);
            }
            _ => {
                let unexpected = char_at(self.b, self.i).unwrap_or(char::REPLACEMENT_CHARACTER);
                self.i += unexpected.len_utf8().min(self.b.len() - self.i);
                self.diags.error(
                    Span::new(lo, self.i as u32),
                    format!("unexpected character '{unexpected}'"),
                );
                TokenKind::Unknown
            }
        };
        Token {
            kind,
            span: Span::new(lo, self.i as u32),
        }
    }

    fn one(&mut self, k: TokenKind) -> TokenKind {
        self.i += 1;
        k
    }
    fn two(&mut self, k: TokenKind) -> TokenKind {
        self.i += 2;
        k
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                b' ' | b'\t' | b'\r' => self.i += 1,
                b'/' if self.peek2() == b'/' => {
                    while self.i < self.b.len() && self.b[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                b'/' if self.peek2() == b'*' => {
                    self.i += 2;
                    while self.i < self.b.len() && !(self.peek() == b'*' && self.peek2() == b'/') {
                        self.i += 1;
                    }
                    self.i = (self.i + 2).min(self.b.len()); // consume */
                }
                // Diagnostic-test markers `<!DIAGNOSTIC_NAME!>` (open) and `<!>` (close) that wrap an
                // expression/declaration in kotlinc's test corpus — strip them as trivia. The close
                // `<!>` is unambiguous. An open marker is only recognized when an UPPER-snake-case name
                // follows AND a closing `!>` exists on the same line — so a real `a < !b` (`<` then
                // unary `!`) is left intact (lowercase/expr operand, no `!>`), never eaten to EOF.
                b'<' if self.peek2() == b'!' && self.peek3() == b'>' => {
                    self.i += 3; // `<!>`
                }
                b'<' if self.peek2() == b'!'
                    && (self.peek3().is_ascii_uppercase() || self.peek3() == b'_') =>
                {
                    let mut j = self.i + 2;
                    while j + 1 < self.b.len()
                        && self.b[j] != b'\n'
                        && !(self.b[j] == b'!' && self.b[j + 1] == b'>')
                    {
                        j += 1;
                    }
                    if j + 1 < self.b.len() && self.b[j] == b'!' && self.b[j + 1] == b'>' {
                        self.i = j + 2; // consume through `!>`
                    } else {
                        break; // not a marker — a real `<` token follows
                    }
                }
                _ => break,
            }
        }
    }

    /// A backtick-quoted identifier (`` `in` ``, `` `is` ``, `` `name with spaces` ``) — Kotlin's escape
    /// for using a keyword or an otherwise-illegal name as an identifier. The token is always an `Ident`
    /// (never re-mapped to a keyword) and its span/text is the CONTENT between the backticks.
    fn backtick_ident(&mut self) -> Token {
        self.i += 1; // opening backtick
        let start = self.i as u32;
        while self.i < self.b.len() && self.b[self.i] != b'`' && self.b[self.i] != b'\n' {
            self.i += 1;
        }
        let end = self.i as u32;
        if self.peek() == b'`' {
            self.i += 1; // closing backtick
        } else {
            // No closing backtick before a newline/EOF — malformed source; report it (the token still
            // becomes the content read so far, so parsing can continue).
            self.diags.error(
                Span::new(start.saturating_sub(1), end),
                "unterminated backtick-quoted identifier".to_string(),
            );
        }
        Token {
            kind: TokenKind::Ident,
            span: Span::new(start, end),
        }
    }

    fn ident(&mut self, lo: u32) -> Token {
        while self.i < self.b.len() && is_ident_continue_at(self.b, self.i) {
            self.i += utf8_char_len(self.b[self.i]);
        }
        let span = Span::new(lo, self.i as u32);
        let text = &std::str::from_utf8(self.b).unwrap()[lo as usize..self.i as usize];
        let kind = keyword(text).unwrap_or(TokenKind::Ident);
        Token { kind, span }
    }

    fn number(&mut self, lo: u32) -> Token {
        // Hex (`0xFF`) / binary (`0b1010`) integer literals (digits, `_` separators, optional `L`).
        if self.b[self.i] == b'0' && matches!(self.peek2(), b'x' | b'X' | b'b' | b'B') {
            self.i += 2; // consume `0x`/`0b`
                         // hex digits (a superset of binary) or `_` separators — stops at the `L` long suffix.
            while self.i < self.b.len()
                && (self.b[self.i].is_ascii_hexdigit() || self.b[self.i] == b'_')
            {
                self.i += 1;
            }
            let kind = if self.peek() == b'L' {
                self.i += 1;
                TokenKind::LongLit
            } else if self.peek() == b'u' || self.peek() == b'U' {
                // `0xFFu` (UInt) / `0xFFuL` (ULong).
                self.i += 1;
                if self.peek() == b'L' || self.peek() == b'l' {
                    self.i += 1;
                    TokenKind::ULongLit
                } else {
                    TokenKind::UIntLit
                }
            } else {
                TokenKind::IntLit
            };
            return Token {
                kind,
                span: Span::new(lo, self.i as u32),
            };
        }
        let mut is_double = false;
        while self.i < self.b.len() && (self.b[self.i].is_ascii_digit() || self.b[self.i] == b'_') {
            self.i += 1;
        }
        if self.peek() == b'.' && self.peek2().is_ascii_digit() {
            is_double = true;
            self.i += 1;
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        // Scientific notation: `1e5`, `1.5E-3`, `9.2E18f`.
        if self.peek() == b'e' || self.peek() == b'E' {
            is_double = true;
            self.i += 1;
            if self.peek() == b'+' || self.peek() == b'-' {
                self.i += 1;
            }
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        let kind = if self.peek() == b'f' || self.peek() == b'F' {
            self.i += 1; // `1.5f` / `1f` — a Float literal
            TokenKind::FloatLit
        } else if (self.peek() == b'u' || self.peek() == b'U') && !is_double {
            // `1u`/`42U` (UInt) and `1uL`/`42UL` (ULong) — unsigned literals.
            self.i += 1; // consume `u`/`U`
            if self.peek() == b'L' || self.peek() == b'l' {
                self.i += 1;
                TokenKind::ULongLit
            } else {
                TokenKind::UIntLit
            }
        } else if self.peek() == b'L' && !is_double {
            self.i += 1;
            TokenKind::LongLit
        } else if is_double {
            if self.peek() == b'd' || self.peek() == b'D' {
                self.i += 1; // optional `d`/`D` suffix on a Double literal
            }
            TokenKind::DoubleLit
        } else {
            TokenKind::IntLit
        };
        Token {
            kind,
            span: Span::new(lo, self.i as u32),
        }
    }

    fn char_lit(&mut self, lo: u32) -> Token {
        self.i += 1; // opening quote
        while self.i < self.b.len() && self.b[self.i] != b'\'' {
            if self.b[self.i] == b'\\' && self.i + 1 < self.b.len() {
                self.i += 2; // escape
            } else {
                self.i += 1;
            }
        }
        if self.i < self.b.len() {
            self.i += 1; // closing quote
        } else {
            self.diags.error(
                Span::new(lo, self.i as u32),
                "unterminated character literal",
            );
        }
        Token {
            kind: TokenKind::CharLit,
            span: Span::new(lo, self.i as u32),
        }
    }

    fn string(&mut self, lo: u32) -> Token {
        if self.peek2() == b'"' && self.b.get(self.i + 2) == Some(&b'"') {
            return self.raw_string(lo);
        }
        if self.string_has_interpolation(self.i, false, 1) {
            return self.string_template(lo, self.i, false, 1);
        }
        self.i += 1; // opening quote
        while self.i < self.b.len() && self.b[self.i] != b'"' {
            if self.b[self.i] == b'\\' && self.i + 1 < self.b.len() {
                self.i += 2; // escape
            } else {
                self.i += 1;
            }
        }
        if self.i < self.b.len() {
            self.i += 1; // closing quote
        } else {
            self.diags
                .error(Span::new(lo, self.i as u32), "unterminated string literal");
        }
        Token {
            kind: TokenKind::StringLit,
            span: Span::new(lo, self.i as u32),
        }
    }

    fn raw_string(&mut self, lo: u32) -> Token {
        if self.string_has_interpolation(self.i, true, 1) {
            return self.string_template(lo, self.i, true, 1);
        }
        self.i += 3; // opening `"""`
        loop {
            if self.i >= self.b.len() {
                self.diags
                    .error(Span::new(lo, self.i as u32), "unterminated string literal");
                break;
            }
            if self.b[self.i] == b'"' {
                let mut q = 0;
                while self.b.get(self.i + q) == Some(&b'"') {
                    q += 1;
                }
                if q >= 3 {
                    self.i += q; // consume the whole quote run; the final three are the delimiter
                    break;
                }
                self.i += q; // a run of one or two quotes is ordinary content
            } else {
                self.i += 1;
            }
        }
        Token {
            kind: TokenKind::StringLit,
            span: Span::new(lo, self.i as u32),
        }
    }

    fn string_has_interpolation(
        &self,
        quote_start: usize,
        raw: bool,
        interpolation_dollars: usize,
    ) -> bool {
        let mut at = quote_start + if raw { 3 } else { 1 };
        while at < self.b.len() {
            if self.b[at] == b'"' {
                let mut quotes = 0;
                while self.b.get(at + quotes) == Some(&b'"') {
                    quotes += 1;
                }
                if !raw || quotes >= 3 {
                    return false;
                }
                at += quotes;
                continue;
            }
            if !raw && self.b[at] == b'\\' {
                at = (at + 2).min(self.b.len());
                continue;
            }
            if self
                .interpolation_marker_at(at, interpolation_dollars)
                .is_some()
            {
                return true;
            }
            at += utf8_char_len(self.b[at]);
        }
        false
    }

    fn string_template(
        &mut self,
        lo: u32,
        quote_start: usize,
        raw: bool,
        interpolation_dollars: usize,
    ) -> Token {
        let mut toks: Vec<Token> = vec![Token {
            kind: if raw {
                TokenKind::RawTemplateStart
            } else {
                TokenKind::TemplateStart
            },
            span: Span::new(lo, (quote_start + if raw { 3 } else { 1 }) as u32),
        }];
        self.i = quote_start + if raw { 3 } else { 1 };
        let mut chunk_lo = self.i;
        loop {
            if self.i >= self.b.len() {
                if self.i > chunk_lo {
                    toks.push(Token {
                        kind: TokenKind::StrChunk,
                        span: Span::new(chunk_lo as u32, self.i as u32),
                    });
                }
                self.diags
                    .error(Span::new(lo, self.i as u32), "unterminated string literal");
                break;
            }
            let c = self.b[self.i];
            if c == b'"' {
                let mut quotes = 0;
                while self.b.get(self.i + quotes) == Some(&b'"') {
                    quotes += 1;
                }
                let closes = if raw { quotes >= 3 } else { true };
                if closes {
                    let content_hi = if raw { self.i + quotes - 3 } else { self.i };
                    if content_hi > chunk_lo {
                        toks.push(Token {
                            kind: TokenKind::StrChunk,
                            span: Span::new(chunk_lo as u32, content_hi as u32),
                        });
                    }
                    self.i += if raw { quotes } else { 1 };
                    break;
                }
                self.i += quotes;
                continue;
            }
            if !raw && c == b'\\' && self.i + 1 < self.b.len() {
                self.i += 2;
                continue;
            }
            if let Some((marker_lo, after_dollars)) =
                self.interpolation_marker_at(self.i, interpolation_dollars)
            {
                if marker_lo > chunk_lo {
                    toks.push(Token {
                        kind: TokenKind::StrChunk,
                        span: Span::new(chunk_lo as u32, marker_lo as u32),
                    });
                }
                self.i = after_dollars;
                toks.push(Token {
                    kind: TokenKind::Dollar,
                    span: Span::new(marker_lo as u32, self.i as u32),
                });
                if self.b[self.i] == b'{' {
                    let lb = self.i;
                    self.i += 1;
                    toks.push(Token {
                        kind: TokenKind::LBrace,
                        span: Span::new(lb as u32, self.i as u32),
                    });
                    let mut depth = 1;
                    loop {
                        let t = self.next_token();
                        if t.kind == TokenKind::Eof {
                            break;
                        }
                        if t.kind == TokenKind::LBrace {
                            depth += 1;
                        } else if t.kind == TokenKind::RBrace {
                            depth -= 1;
                            if depth == 0 {
                                toks.push(t);
                                break;
                            }
                        }
                        toks.push(t);
                    }
                } else if self.b[self.i] == b'`' {
                    toks.push(self.backtick_ident());
                } else {
                    let id_lo = self.i;
                    while self.i < self.b.len() && is_ident_continue_at(self.b, self.i) {
                        self.i += utf8_char_len(self.b[self.i]);
                    }
                    toks.push(Token {
                        kind: TokenKind::Ident,
                        span: Span::new(id_lo as u32, self.i as u32),
                    });
                }
                chunk_lo = self.i;
            } else if c == b'$' {
                while self.b.get(self.i) == Some(&b'$') {
                    self.i += 1;
                }
            } else {
                self.i += utf8_char_len(c);
            }
        }
        toks.push(Token {
            kind: TokenKind::TemplateEnd,
            span: Span::new(self.i as u32, self.i as u32),
        });
        let first = toks.remove(0);
        self.pending.extend(toks);
        first
    }

    fn multi_dollar_prefix_len(&self) -> Option<usize> {
        let mut quote = self.i;
        while self.b.get(quote) == Some(&b'$') {
            quote += 1;
        }
        (quote > self.i && self.b.get(quote) == Some(&b'"')).then_some(quote - self.i)
    }

    fn prefixed_string(&mut self, lo: u32, dollars: usize) -> Token {
        let quote_start = self.i + dollars;
        let raw = self.b.get(quote_start + 1) == Some(&b'"')
            && self.b.get(quote_start + 2) == Some(&b'"');
        self.string_template(lo, quote_start, raw, dollars)
    }

    fn interpolation_marker_at(&self, at: usize, required: usize) -> Option<(usize, usize)> {
        if self.b.get(at) != Some(&b'$') {
            return None;
        }
        let mut after = at;
        while self.b.get(after) == Some(&b'$') {
            after += 1;
        }
        if after - at < required {
            return None;
        }
        self.b
            .get(after)
            .is_some_and(|next| *next == b'{' || *next == b'`' || is_ident_start_at(self.b, after))
            .then_some((after - required, after))
    }
}

fn is_ascii_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn is_ident_start_at(bytes: &[u8], index: usize) -> bool {
    bytes.get(index).is_some_and(|byte| {
        is_ascii_ident_start(*byte)
            || (*byte >= 0x80 && char_at(bytes, index).is_some_and(is_kotlin_letter))
    })
}

fn is_ident_continue_at(bytes: &[u8], index: usize) -> bool {
    bytes.get(index).is_some_and(|byte| {
        *byte == b'_'
            || byte.is_ascii_alphanumeric()
            || (*byte >= 0x80
                && char_at(bytes, index).is_some_and(|character| {
                    is_kotlin_letter(character)
                        || get_general_category(character) == GeneralCategory::DecimalNumber
                }))
    })
}

fn is_kotlin_letter(character: char) -> bool {
    matches!(
        get_general_category(character),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
    )
}

fn char_at(bytes: &[u8], index: usize) -> Option<char> {
    let length = utf8_char_len(*bytes.get(index)?);
    std::str::from_utf8(bytes.get(index..index.checked_add(length)?)?)
        .ok()?
        .chars()
        .next()
}

fn utf8_char_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let mut d = DiagSink::new();
        lex(src, &mut d)
            .into_iter()
            .map(|t| t.kind)
            .filter(|k| *k != TokenKind::Newline)
            .collect()
    }

    #[test]
    fn function_signature() {
        use TokenKind::*;
        let k = kinds("fun f(a: Int, b: String): String = a");
        assert_eq!(
            k,
            vec![
                KwFun, Ident, LParen, Ident, Colon, Ident, Comma, Ident, Colon, Ident, RParen,
                Colon, Ident, Eq, Ident, Eof
            ]
        );
    }

    #[test]
    fn literals() {
        use TokenKind::*;
        assert_eq!(kinds("123"), vec![IntLit, Eof]);
        assert_eq!(kinds("123L"), vec![LongLit, Eof]);
        assert_eq!(kinds("1.5"), vec![DoubleLit, Eof]);
        assert_eq!(kinds("\"hi\\n\""), vec![StringLit, Eof]);
        assert_eq!(kinds("true false"), vec![KwTrue, KwFalse, Eof]);
    }

    #[test]
    fn operators_multichar() {
        use TokenKind::*;
        assert_eq!(
            kinds("== != <= >= && || ! = < >"),
            vec![EqEq, NotEq, LtEq, GtEq, AndAnd, OrOr, Not, Eq, Lt, Gt, Eof]
        );
    }

    #[test]
    fn comments_and_layout() {
        use TokenKind::*;
        let k = kinds("val x // line\n /* block */ = 1");
        assert_eq!(k, vec![KwVal, Ident, Eq, IntLit, Eof]);
    }

    #[test]
    fn newlines_emitted() {
        let mut d = DiagSink::new();
        let toks = lex("a\nb", &mut d);
        assert!(toks.iter().any(|t| t.kind == TokenKind::Newline));
        assert!(!d.has_errors());
    }

    #[test]
    fn member_call_dotted() {
        use TokenKind::*;
        // `a.toString()` — Dot must not be confused with a double literal.
        assert_eq!(
            kinds("a.toString()"),
            vec![Ident, Dot, Ident, LParen, RParen, Eof]
        );
    }

    #[test]
    fn name_lexer_uses_the_same_identifiers_without_retaining_other_tokens() {
        use NameTokenKind::*;
        let mut diagnostics = DiagSink::new();
        let tokens = lex_name_tokens(
            "package demo\n@Deprecated fun greet(name: String) = \"hi $name\"",
            &mut diagnostics,
        );
        let kinds: Vec<_> = tokens.iter().map(|token| token.kind).collect();
        assert_eq!(
            kinds,
            vec![Package, Ident, Newline, At, Ident, Ident, Ident, Ident, Ident]
        );
        assert!(!diagnostics.has_errors());
        assert!(tokens.len() < lex("fun greet(name: String) = 1", &mut diagnostics).len());
    }

    #[test]
    fn name_lexer_retains_compound_assignment_operators_for_editor_highlighting() {
        let source = "a += b; c -= d; e *= f; g /= h; i %= j";
        let mut diagnostics = DiagSink::new();
        let operators = lex_name_tokens(source, &mut diagnostics)
            .into_iter()
            .filter(|token| token.kind == NameTokenKind::Operator)
            .map(|token| token.text(source))
            .collect::<Vec<_>>();

        assert_eq!(operators, ["+=", "-=", "*=", "/=", "%="]);
        assert!(!diagnostics.has_errors());
    }

    #[test]
    fn unicode_identifiers_keep_utf8_byte_spans_in_full_and_name_lexers() {
        let source = "fun unicode(π: String) = π";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let identifiers = tokens
            .iter()
            .filter(|token| token.kind == TokenKind::Ident)
            .map(|token| {
                (
                    token.span,
                    &source[token.span.lo as usize..token.span.hi as usize],
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            identifiers,
            vec![
                (Span::new(4, 11), "unicode"),
                (Span::new(12, 14), "π"),
                (Span::new(16, 22), "String"),
                (Span::new(26, 28), "π"),
            ]
        );
        assert!(!diagnostics.has_errors());

        let names = lex_name_tokens(source, &mut diagnostics);
        assert_eq!(
            names
                .iter()
                .filter(|token| token.kind == NameTokenKind::Ident)
                .map(|token| token.text(source))
                .collect::<Vec<_>>(),
            vec!["unicode", "π", "String", "π"]
        );
        assert!(!diagnostics.has_errors());
    }

    #[test]
    fn unicode_identifiers_follow_kotlin_categories_and_exact_utf8_spans() {
        use TokenKind::*;

        // Arabic-Indic ٢ is Nd and may continue an identifier. Roman Ⅻ is Nl,
        // superscript ² is No, and the combining acute accent is Mn; Kotlin's
        // ordinary identifier grammar admits none of those three categories.
        let source = "π٢ Ⅻ ² a\u{301}";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        assert_eq!(
            tokens
                .iter()
                .map(|token| (token.kind, token.span))
                .collect::<Vec<_>>(),
            vec![
                (Ident, Span::new(0, 4)),
                (Unknown, Span::new(5, 8)),
                (Unknown, Span::new(9, 11)),
                (Ident, Span::new(12, 13)),
                (Unknown, Span::new(13, 15)),
                (Eof, Span::new(15, 15)),
            ]
        );
        assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| (diagnostic.span, diagnostic.msg.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (Span::new(5, 8), "unexpected character 'Ⅻ'"),
                (Span::new(9, 11), "unexpected character '²'"),
                (Span::new(13, 15), "unexpected character '\u{301}'"),
            ]
        );
    }

    #[test]
    fn unicode_identifier_categories_are_shared_with_string_templates() {
        let source = "\"value=$π٢\"";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let identifiers = tokens
            .iter()
            .filter(|token| token.kind == TokenKind::Ident)
            .map(|token| {
                (
                    token.span,
                    &source[token.span.lo as usize..token.span.hi as usize],
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(identifiers, vec![(Span::new(8, 12), "π٢")]);
        assert!(!diagnostics.has_errors());
    }

    #[test]
    fn multi_dollar_marker_uses_the_end_of_a_dollar_run() {
        use TokenKind::*;

        let source = r#"$$"a$short$$name$$$`when`""#;
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let actual = tokens
            .iter()
            .map(|token| (token.kind, token.text(source)))
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            vec![
                (TemplateStart, "$$\""),
                (StrChunk, "a$short"),
                (Dollar, "$$"),
                (Ident, "name"),
                (StrChunk, "$"),
                (Dollar, "$$"),
                (Ident, "when"),
                (TemplateEnd, ""),
                (Eof, ""),
            ]
        );
        assert!(!diagnostics.has_errors());
    }

    #[test]
    fn raw_template_keeps_quotes_before_the_closing_delimiter() {
        use TokenKind::*;

        let source = "$$\"\"\"[$$name]\"\"\"\"\"";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let actual = tokens
            .iter()
            .map(|token| (token.kind, token.text(source)))
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            vec![
                (RawTemplateStart, "$$\"\"\""),
                (StrChunk, "["),
                (Dollar, "$$"),
                (Ident, "name"),
                (StrChunk, "]\"\""),
                (TemplateEnd, ""),
                (Eof, ""),
            ]
        );
        assert!(!diagnostics.has_errors());
    }
}
