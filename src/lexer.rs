//! Stage A: byte slice → token stream.
//!
//! Newlines are emitted as `Newline` tokens (Kotlin uses them as statement separators); other
//! whitespace and comments are skipped. The lexer never fails — unknown bytes become
//! `TokenKind::Unknown` with a diagnostic, so later stages can still make progress.

use crate::diag::{DiagSink, Span};
use crate::token::{keyword, Token, TokenKind};

pub fn lex(src: &str, diags: &mut DiagSink) -> Vec<Token> {
    Lexer { b: src.as_bytes(), i: 0, out: Vec::new(), diags }.run()
}

struct Lexer<'a> {
    b: &'a [u8],
    i: usize,
    out: Vec<Token>,
    diags: &'a mut DiagSink,
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

    fn peek(&self) -> u8 {
        if self.i < self.b.len() { self.b[self.i] } else { 0 }
    }
    fn peek2(&self) -> u8 {
        if self.i + 1 < self.b.len() { self.b[self.i + 1] } else { 0 }
    }

    fn next_token(&mut self) -> Token {
        self.skip_trivia();
        let lo = self.i as u32;
        if self.i >= self.b.len() {
            return Token { kind: TokenKind::Eof, span: Span::new(lo, lo) };
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
            b':' => self.one(TokenKind::Colon),
            b'.' if self.peek2() == b'.' => self.two(TokenKind::DotDot),
            b'.' if !self.peek2().is_ascii_digit() => self.one(TokenKind::Dot),
            b'+' if self.peek2() == b'=' => self.two(TokenKind::PlusEq),
            b'+' => self.one(TokenKind::Plus),
            b'-' if self.peek2() == b'>' => self.two(TokenKind::Arrow),
            b'-' if self.peek2() == b'=' => self.two(TokenKind::MinusEq),
            b'-' => self.one(TokenKind::Minus),
            b'*' if self.peek2() == b'=' => self.two(TokenKind::StarEq),
            b'*' => self.one(TokenKind::Star),
            b'/' if self.peek2() == b'=' => self.two(TokenKind::SlashEq),
            b'/' => self.one(TokenKind::Slash),
            b'%' if self.peek2() == b'=' => self.two(TokenKind::PercentEq),
            b'%' => self.one(TokenKind::Percent),
            b'=' if self.peek2() == b'=' => self.two(TokenKind::EqEq),
            b'=' => self.one(TokenKind::Eq),
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
            b'0'..=b'9' => return self.number(lo),
            b'.' => return self.number(lo), // .5
            c if is_ident_start(c) => return self.ident(lo),
            _ => {
                self.i += 1;
                self.diags.error(Span::new(lo, self.i as u32), format!("unexpected character '{}'", c as char));
                TokenKind::Unknown
            }
        };
        Token { kind, span: Span::new(lo, self.i as u32) }
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
                _ => break,
            }
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
        let mut is_double = false;
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        if self.peek() == b'.' && self.peek2().is_ascii_digit() {
            is_double = true;
            self.i += 1;
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        let kind = if self.peek() == b'L' && !is_double {
            self.i += 1;
            TokenKind::LongLit
        } else if is_double {
            TokenKind::DoubleLit
        } else {
            TokenKind::IntLit
        };
        Token { kind, span: Span::new(lo, self.i as u32) }
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
            self.diags.error(Span::new(lo, self.i as u32), "unterminated character literal");
        }
        Token { kind: TokenKind::CharLit, span: Span::new(lo, self.i as u32) }
    }

    fn string(&mut self, lo: u32) -> Token {
        // Raw strings (`"""..."""`) and string templates (`"$x"`, `"${...}"`) are outside the
        // supported subset — reject them so callers don't silently miscompile (the differential
        // harness relies on krusty refusing what it can't compile correctly).
        if self.peek2() == b'"' && self.b.get(self.i + 2) == Some(&b'"') {
            self.i += 3;
            self.diags.error(Span::new(lo, self.i as u32), "raw string literals are not supported");
            return Token { kind: TokenKind::StringLit, span: Span::new(lo, self.i as u32) };
        }
        self.i += 1; // opening quote
        while self.i < self.b.len() && self.b[self.i] != b'"' {
            if self.b[self.i] == b'\\' && self.i + 1 < self.b.len() {
                self.i += 2; // escape
            } else {
                if self.b[self.i] == b'$' {
                    let next = self.b.get(self.i + 1).copied().unwrap_or(0);
                    if next == b'{' || is_ident_start(next) {
                        self.diags.error(Span::new(lo, self.i as u32), "string templates are not supported");
                    }
                }
                self.i += 1;
            }
        }
        if self.i < self.b.len() {
            self.i += 1; // closing quote
        } else {
            self.diags.error(Span::new(lo, self.i as u32), "unterminated string literal");
        }
        Token { kind: TokenKind::StringLit, span: Span::new(lo, self.i as u32) }
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
        lex(src, &mut d).into_iter().map(|t| t.kind).filter(|k| *k != TokenKind::Newline).collect()
    }

    #[test]
    fn function_signature() {
        use TokenKind::*;
        let k = kinds("fun f(a: Int, b: String): String = a");
        assert_eq!(
            k,
            vec![KwFun, Ident, LParen, Ident, Colon, Ident, Comma, Ident, Colon, Ident, RParen, Colon, Ident, Eq, Ident, Eof]
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
        assert_eq!(kinds("a.toString()"), vec![Ident, Dot, Ident, LParen, RParen, Eof]);
    }
}
