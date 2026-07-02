//! Stage A: byte slice → token stream.
//!
//! Newlines are emitted as `Newline` tokens (Kotlin uses them as statement separators); other
//! whitespace and comments are skipped. The lexer never fails — unknown bytes become
//! `TokenKind::Unknown` with a diagnostic, so later stages can still make progress.

use crate::diag::{DiagSink, Span};
use crate::token::{keyword, Token, TokenKind};

pub fn lex(src: &str, diags: &mut DiagSink) -> Vec<Token> {
    Lexer {
        b: src.as_bytes(),
        i: 0,
        out: Vec::new(),
        diags,
        pending: std::collections::VecDeque::new(),
    }
    .run()
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
            b'|' if self.peek2() == b'|' => self.two(TokenKind::OrOr),
            b'"' => return self.string(lo),
            b'\'' => return self.char_lit(lo),
            b'`' => return self.backtick_ident(),
            b'0'..=b'9' => return self.number(lo),
            b'.' => return self.number(lo), // .5
            c if is_ident_start(c) => return self.ident(lo),
            _ => {
                self.i += 1;
                self.diags.error(
                    Span::new(lo, self.i as u32),
                    format!("unexpected character '{}'", c as char),
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
        while self.i < self.b.len() && is_ident_continue(self.b[self.i]) {
            self.i += 1;
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
        // Raw strings (`"""..."""`): no escape processing, may span lines. Interpolation (`$x`/`${}`)
        // is not yet supported — reject (skip) rather than mis-lex it as literal text.
        if self.peek2() == b'"' && self.b.get(self.i + 2) == Some(&b'"') {
            return self.raw_string(lo);
        }
        if self.string_has_interpolation() {
            return self.string_template(lo);
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

    /// Lex a raw string `"""..."""`. Content is verbatim (no escapes); the closing delimiter is a run
    /// of three quotes (a run of >3 leaves the surplus quotes in the content). The emitted `StringLit`
    /// token spans the whole literal; the parser strips the three leading/trailing quotes.
    fn raw_string(&mut self, lo: u32) -> Token {
        self.i += 3; // opening `"""`
        let content_lo = self.i;
        let close_start;
        loop {
            if self.i >= self.b.len() {
                self.diags
                    .error(Span::new(lo, self.i as u32), "unterminated string literal");
                close_start = self.i;
                break;
            }
            if self.b[self.i] == b'"' {
                let mut q = 0;
                while self.b.get(self.i + q) == Some(&b'"') {
                    q += 1;
                }
                if q >= 3 {
                    close_start = self.i;
                    self.i += q; // consume the whole quote run; the final three are the delimiter
                    break;
                }
                self.i += q; // a run of one or two quotes is ordinary content
            } else {
                self.i += 1;
            }
        }
        // Interpolation inside a raw string isn't supported yet — reject so it isn't taken literally.
        let mut j = content_lo;
        while j < close_start {
            if self.b[j] == b'$' {
                let next = self.b.get(j + 1).copied().unwrap_or(0);
                if next == b'{' || is_ident_start(next) {
                    self.diags.error(
                        Span::new(lo, self.i as u32),
                        "raw string interpolation is not supported",
                    );
                    break;
                }
            }
            j += 1;
        }
        Token {
            kind: TokenKind::StringLit,
            span: Span::new(lo, self.i as u32),
        }
    }

    /// Does the string starting at `self.i` (`"`) contain a `$ident` / `${` interpolation?
    fn string_has_interpolation(&self) -> bool {
        let mut j = self.i + 1;
        while j < self.b.len() && self.b[j] != b'"' {
            if self.b[j] == b'\\' {
                j += 2;
                continue;
            }
            if self.b[j] == b'$' {
                let next = self.b.get(j + 1).copied().unwrap_or(0);
                if next == b'{' || is_ident_start(next) {
                    return true;
                }
            }
            j += 1;
        }
        false
    }

    /// Lex an interpolated string into the token sequence
    /// `TemplateStart (StrChunk | Dollar Ident | Dollar LBrace <expr> RBrace)* TemplateEnd`,
    /// returning the first token and queueing the rest.
    fn string_template(&mut self, lo: u32) -> Token {
        let mut toks: Vec<Token> = vec![Token {
            kind: TokenKind::TemplateStart,
            span: Span::new(lo, lo + 1),
        }];
        self.i += 1; // opening quote
        let mut chunk_lo = self.i;
        while self.i < self.b.len() && self.b[self.i] != b'"' {
            let c = self.b[self.i];
            if c == b'\\' && self.i + 1 < self.b.len() {
                self.i += 2;
                continue;
            }
            let next = self.b.get(self.i + 1).copied().unwrap_or(0);
            if c == b'$' && (next == b'{' || is_ident_start(next)) {
                if self.i > chunk_lo {
                    toks.push(Token {
                        kind: TokenKind::StrChunk,
                        span: Span::new(chunk_lo as u32, self.i as u32),
                    });
                }
                let dollar_lo = self.i;
                self.i += 1; // consume `$`
                toks.push(Token {
                    kind: TokenKind::Dollar,
                    span: Span::new(dollar_lo as u32, self.i as u32),
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
                        let t = self.lex_one();
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
                } else {
                    let id_lo = self.i;
                    while self.i < self.b.len() && is_ident_continue(self.b[self.i]) {
                        self.i += 1;
                    }
                    toks.push(Token {
                        kind: TokenKind::Ident,
                        span: Span::new(id_lo as u32, self.i as u32),
                    });
                }
                chunk_lo = self.i;
            } else {
                self.i += 1;
            }
        }
        if self.i > chunk_lo {
            toks.push(Token {
                kind: TokenKind::StrChunk,
                span: Span::new(chunk_lo as u32, self.i as u32),
            });
        }
        if self.i < self.b.len() {
            self.i += 1; // closing quote
        } else {
            self.diags
                .error(Span::new(lo, self.i as u32), "unterminated string literal");
        }
        toks.push(Token {
            kind: TokenKind::TemplateEnd,
            span: Span::new(self.i as u32, self.i as u32),
        });
        let first = toks.remove(0);
        self.pending.extend(toks);
        first
    }
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}
fn is_ident_continue(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
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

    // --- helpers ---------------------------------------------------------

    fn lex_ok(src: &str) -> Vec<Token> {
        let mut d = DiagSink::new();
        let toks = lex(src, &mut d);
        assert!(!d.has_errors(), "unexpected diagnostics for {src:?}");
        toks
    }

    fn one_kind(src: &str) -> TokenKind {
        let k = kinds(src);
        assert_eq!(k.len(), 2, "expected one token + Eof for {src:?}: {k:?}");
        assert_eq!(k[1], TokenKind::Eof);
        k[0]
    }

    // --- number literals -------------------------------------------------

    #[test]
    fn integer_and_suffix_forms() {
        use TokenKind::*;
        assert_eq!(one_kind("0"), IntLit);
        assert_eq!(one_kind("42"), IntLit);
        assert_eq!(one_kind("1_000_000"), IntLit);
        assert_eq!(one_kind("123L"), LongLit);
        assert_eq!(one_kind("42u"), UIntLit);
        assert_eq!(one_kind("42U"), UIntLit);
        assert_eq!(one_kind("42uL"), ULongLit);
        assert_eq!(one_kind("42UL"), ULongLit);
    }

    #[test]
    fn hex_and_binary_literals() {
        use TokenKind::*;
        assert_eq!(one_kind("0xFF"), IntLit);
        assert_eq!(one_kind("0Xff"), IntLit);
        assert_eq!(one_kind("0xDE_AD"), IntLit);
        assert_eq!(one_kind("0xFFL"), LongLit);
        assert_eq!(one_kind("0xFFu"), UIntLit);
        assert_eq!(one_kind("0xFFuL"), ULongLit);
        assert_eq!(one_kind("0b1010"), IntLit);
        assert_eq!(one_kind("0B1101"), IntLit);
    }

    #[test]
    fn floating_point_forms() {
        use TokenKind::*;
        assert_eq!(one_kind("1.5"), DoubleLit);
        assert_eq!(one_kind(".5"), DoubleLit); // leading-dot double
        assert_eq!(one_kind("1.5f"), FloatLit);
        assert_eq!(one_kind("1f"), FloatLit);
        assert_eq!(one_kind("1F"), FloatLit);
        assert_eq!(one_kind("1.5d"), DoubleLit); // optional d suffix
        assert_eq!(one_kind("1e5"), DoubleLit);
        assert_eq!(one_kind("1.5E-3"), DoubleLit);
        assert_eq!(one_kind("9.2E18f"), FloatLit); // scientific + float suffix
    }

    #[test]
    fn dot_after_number_is_range_not_double() {
        use TokenKind::*;
        // `1..2` is a range, not `1.` `.2`.
        assert_eq!(kinds("1..2"), vec![IntLit, DotDot, IntLit, Eof]);
        assert_eq!(kinds("1..<2"), vec![IntLit, DotDotLt, IntLit, Eof]);
    }

    // --- operators -------------------------------------------------------

    #[test]
    fn compound_assign_and_increment() {
        use TokenKind::*;
        assert_eq!(
            kinds("+= -= *= /= %= ++ --"),
            vec![PlusEq, MinusEq, StarEq, SlashEq, PercentEq, PlusPlus, MinusMinus, Eof]
        );
    }

    #[test]
    fn reference_equality_and_arrows() {
        use TokenKind::*;
        assert_eq!(
            kinds("=== !== -> :: .. ..<"),
            vec![RefEq, RefNe, Arrow, ColonColon, DotDot, DotDotLt, Eof]
        );
    }

    #[test]
    fn misc_single_char_ops() {
        use TokenKind::*;
        assert_eq!(
            kinds("? @ : . [ ] { } ( ) , % *"),
            vec![
                Question, At, Colon, Dot, LBracket, RBracket, LBrace, RBrace, LParen, RParen,
                Comma, Percent, Star, Eof
            ]
        );
    }

    #[test]
    fn not_not_is_two_nots() {
        use TokenKind::*;
        // `a!!` — the not-null assertion is lexed as two `Not` tokens (postfix).
        assert_eq!(kinds("a!!"), vec![Ident, Not, Not, Eof]);
    }

    // --- identifiers & keywords -----------------------------------------

    #[test]
    fn keywords_recognized() {
        use TokenKind::*;
        assert_eq!(
            kinds("fun class val var return if else when while do for in true false null package import"),
            vec![
                KwFun, KwClass, KwVal, KwVar, KwReturn, KwIf, KwElse, KwWhen, KwWhile, KwDo, KwFor,
                KwIn, KwTrue, KwFalse, KwNull, KwPackage, KwImport, Eof
            ]
        );
    }

    #[test]
    fn type_names_are_plain_idents() {
        use TokenKind::*;
        // Type names are NOT keywords — they stay identifiers.
        assert_eq!(kinds("Int String Boolean"), vec![Ident, Ident, Ident, Eof]);
    }

    #[test]
    fn identifier_underscore_and_digits() {
        use TokenKind::*;
        assert_eq!(one_kind("_foo"), Ident);
        assert_eq!(one_kind("a1_b2"), Ident);
        assert_eq!(one_kind("_"), Ident);
    }

    #[test]
    fn ident_text_and_span() {
        let src = "hello world";
        let toks = lex_ok(src);
        assert_eq!(toks[0].text(src), "hello");
        assert_eq!(toks[0].span, Span::new(0, 5));
        // second non-eof, non-newline token
        let second = toks.iter().find(|t| t.span.lo == 6).unwrap();
        assert_eq!(second.text(src), "world");
    }

    // --- backtick identifiers -------------------------------------------

    #[test]
    fn backtick_keyword_becomes_ident() {
        let src = "`in`";
        let toks = lex_ok(src);
        assert_eq!(toks[0].kind, TokenKind::Ident);
        // content between backticks (span excludes them)
        assert_eq!(toks[0].text(src), "in");
        assert_eq!(toks[0].span, Span::new(1, 3));
    }

    #[test]
    fn backtick_name_with_spaces() {
        let src = "`name with spaces`";
        let toks = lex_ok(src);
        assert_eq!(toks[0].kind, TokenKind::Ident);
        assert_eq!(toks[0].text(src), "name with spaces");
    }

    #[test]
    fn unterminated_backtick_reports_error() {
        let mut d = DiagSink::new();
        let toks = lex("`oops\n", &mut d);
        assert!(d.has_errors());
        assert_eq!(toks[0].kind, TokenKind::Ident);
    }

    // --- strings & chars -------------------------------------------------

    #[test]
    fn plain_and_escaped_strings() {
        use TokenKind::*;
        assert_eq!(one_kind("\"hi\""), StringLit);
        assert_eq!(one_kind("\"a\\\"b\""), StringLit); // embedded escaped quote
        assert_eq!(one_kind("\"\""), StringLit); // empty
    }

    #[test]
    fn unterminated_string_reports_error() {
        let mut d = DiagSink::new();
        let toks = lex("\"oops", &mut d);
        assert!(d.has_errors());
        assert_eq!(toks[0].kind, TokenKind::StringLit);
    }

    #[test]
    fn raw_string_spans_lines_verbatim() {
        let src = "\"\"\"a\nb\"\"\"";
        let toks = lex_ok(src);
        assert_eq!(toks[0].kind, TokenKind::StringLit);
        assert_eq!(toks[0].text(src), src); // whole literal incl. triple quotes
    }

    #[test]
    fn raw_string_with_interior_quotes() {
        let src = "\"\"\"a\"b\"\"\"";
        let toks = lex_ok(src);
        assert_eq!(toks[0].kind, TokenKind::StringLit);
    }

    #[test]
    fn raw_string_interpolation_rejected() {
        let mut d = DiagSink::new();
        lex("\"\"\"$x\"\"\"", &mut d);
        assert!(d.has_errors());
    }

    #[test]
    fn char_literals() {
        use TokenKind::*;
        assert_eq!(one_kind("'x'"), CharLit);
        assert_eq!(one_kind("'\\n'"), CharLit);
        assert_eq!(one_kind("'\\''"), CharLit); // escaped quote
    }

    #[test]
    fn unterminated_char_reports_error() {
        let mut d = DiagSink::new();
        let toks = lex("'x", &mut d);
        assert!(d.has_errors());
        assert_eq!(toks[0].kind, TokenKind::CharLit);
    }

    // --- string templates -----------------------------------------------

    #[test]
    fn template_simple_interpolation() {
        use TokenKind::*;
        assert_eq!(
            kinds("\"$x\""),
            vec![TemplateStart, Dollar, Ident, TemplateEnd, Eof]
        );
    }

    #[test]
    fn template_block_interpolation() {
        use TokenKind::*;
        assert_eq!(
            kinds("\"a${b}c\""),
            vec![
                TemplateStart,
                StrChunk,
                Dollar,
                LBrace,
                Ident,
                RBrace,
                StrChunk,
                TemplateEnd,
                Eof
            ]
        );
    }

    #[test]
    fn template_nested_braces() {
        use TokenKind::*;
        // `${ f({}) }` — inner braces must balance before the template closes.
        let k = kinds("\"${f({})}\"");
        assert_eq!(k.first(), Some(&TemplateStart));
        assert_eq!(k[k.len() - 2], TemplateEnd);
    }

    #[test]
    fn dollar_without_interpolation_is_plain_string() {
        use TokenKind::*;
        // `$` not followed by an ident/`{` stays a literal string, no template.
        assert_eq!(one_kind("\"cost is $5\""), StringLit);
    }

    // --- comments & trivia ----------------------------------------------

    #[test]
    fn line_and_block_comments_skipped() {
        use TokenKind::*;
        assert_eq!(kinds("a // comment\nb"), vec![Ident, Ident, Eof]);
        assert_eq!(kinds("a /* c */ b"), vec![Ident, Ident, Eof]);
    }

    #[test]
    fn unterminated_block_comment_eats_to_eof() {
        use TokenKind::*;
        assert_eq!(kinds("a /* unterminated"), vec![Ident, Eof]);
    }

    #[test]
    fn semicolon_is_a_newline() {
        let mut d = DiagSink::new();
        let toks = lex("a;b", &mut d);
        let ks: Vec<_> = toks.iter().map(|t| t.kind).collect();
        assert_eq!(
            ks,
            vec![
                TokenKind::Ident,
                TokenKind::Newline,
                TokenKind::Ident,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn newlines_are_tokens_but_whitespace_is_not() {
        let mut d = DiagSink::new();
        let toks = lex("a\n\nb", &mut d);
        let n = toks.iter().filter(|t| t.kind == TokenKind::Newline).count();
        assert_eq!(n, 2);
    }

    // --- diagnostic markers (kotlinc corpus) ----------------------------

    #[test]
    fn diagnostic_markers_stripped_as_trivia() {
        use TokenKind::*;
        // `<!NAME!> ... <!>` wrap markers are stripped, leaving the wrapped tokens.
        assert_eq!(kinds("<!FOO!>x<!>"), vec![Ident, Eof]);
        assert_eq!(kinds("<!ERR_NAME!>1<!>"), vec![IntLit, Eof]);
    }

    #[test]
    fn real_less_than_not_treated_as_marker() {
        use TokenKind::*;
        // `a < !b` — lowercase operand, no `!>`: a genuine `<` then unary `!`.
        assert_eq!(kinds("a < !b"), vec![Ident, Lt, Not, Ident, Eof]);
    }

    // --- unknown input ---------------------------------------------------

    #[test]
    fn unknown_char_produces_diagnostic() {
        let mut d = DiagSink::new();
        let toks = lex("#", &mut d);
        assert!(d.has_errors());
        assert_eq!(toks[0].kind, TokenKind::Unknown);
    }

    #[test]
    fn empty_input_is_just_eof() {
        assert_eq!(kinds(""), vec![TokenKind::Eof]);
    }

    #[test]
    fn stream_always_ends_with_eof() {
        for src in ["", "a", "1 + 2", "fun f() {}", "\"s\"", "// c"] {
            let toks = lex_ok(src);
            assert_eq!(toks.last().unwrap().kind, TokenKind::Eof);
        }
    }
}
