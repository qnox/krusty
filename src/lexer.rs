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
                        // `next_token` (not `lex_one`) so a NESTED string template inside this `${…}`
                        // — which `string_template` expands by queueing its tokens onto `self.pending`
                        // — is consumed in order here; `lex_one` would skip the queued inner tokens.
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
}
