//! The token-only file preamble shared by syntax parsing and lightweight source scans.
//!
//! Import lists precede declarations and script statements. A separator is optional, so an
//! import may follow a package or another import on the same line. An ordinary expression
//! named `import` does not open a directive. This reader never enters a declaration body.

use crate::diag::Span;
use crate::token::{Token, TokenKind as K};

pub(crate) struct Import {
    pub path: String,
    pub wildcard: bool,
    pub imported_name: Option<String>,
}

pub(crate) struct Header {
    pub package: Option<String>,
    pub imports: Vec<Import>,
    pub import_starts: Vec<u32>,
    /// Every token requested from the stream, including lookahead at its first body item.
    /// A streaming consumer emits this prefix before continuing the same lexer.
    pub prefix: Vec<Token>,
}

pub(crate) fn from_tokens(source: &str, tokens: &[Token]) -> Header {
    let mut tokens = tokens.iter().copied();
    read(source, || {
        tokens.next().unwrap_or(Token {
            kind: K::Eof,
            span: Span::new(source.len() as u32, source.len() as u32),
        })
    })
}

pub(crate) fn read(source: &str, next: impl FnMut() -> Token) -> Header {
    let mut cursor = Cursor {
        source,
        next,
        prefix: Vec::new(),
        at: 0,
    };
    cursor.newlines();
    let package_opens_file = cursor.peek(0).kind == K::KwPackage;
    let mut header = Header {
        package: None,
        imports: Vec::new(),
        import_starts: Vec::new(),
        prefix: Vec::new(),
    };
    let mut first_package = true;
    loop {
        cursor.newlines();
        if cursor.file_annotation() {
            continue;
        }
        if cursor.peek(0).kind == K::KwPackage {
            cursor.bump();
            let path = cursor.path().join(".");
            if first_package && package_opens_file {
                header.package = Some(path);
            } else if header.package.is_none() && !path.is_empty() {
                header.package = Some(path);
            }
            first_package = false;
            continue;
        }
        let keyword = cursor.peek(0);
        if !bare(source, keyword, "import") || cursor.peek(1).kind != K::Ident {
            break;
        }
        cursor.bump();
        header.import_starts.push(keyword.span.lo);
        let path = cursor.path();
        let wildcard = cursor.peek(0).kind == K::Star;
        if wildcard {
            cursor.bump();
        }
        let mut alias = None;
        if bare(source, cursor.peek(0), "as") {
            cursor.bump();
            if cursor.peek(0).kind == K::Ident {
                alias = Some(cursor.bump().text(source).to_owned());
            }
        }
        let imported_name = if wildcard {
            None
        } else {
            alias.or_else(|| path.last().cloned())
        };
        header.imports.push(Import {
            path: path.join("."),
            wildcard,
            imported_name,
        });
    }
    header.prefix = cursor.prefix;
    header
}

fn bare(source: &str, token: Token, text: &str) -> bool {
    token.kind == K::Ident
        && token.text(source) == text
        && !(token.span.lo > 0
            && source.as_bytes().get(token.span.lo as usize - 1) == Some(&b'`')
            && source.as_bytes().get(token.span.hi as usize) == Some(&b'`'))
}

struct Cursor<'a, F> {
    source: &'a str,
    next: F,
    prefix: Vec<Token>,
    at: usize,
}

impl<F: FnMut() -> Token> Cursor<'_, F> {
    fn peek(&mut self, ahead: usize) -> Token {
        while self.prefix.len() <= self.at + ahead {
            self.prefix.push((self.next)());
        }
        self.prefix[self.at + ahead]
    }

    fn bump(&mut self) -> Token {
        let token = self.peek(0);
        self.at += 1;
        token
    }

    fn newlines(&mut self) {
        while self.peek(0).kind == K::Newline {
            self.bump();
        }
    }

    // Keep the parser's qualified-path consumption, including a trailing dot before `*`.
    fn path(&mut self) -> Vec<String> {
        let mut path = Vec::new();
        if self.peek(0).kind == K::Ident {
            let token = self.bump();
            path.push(token.text(self.source).to_owned());
            while self.peek(0).kind == K::Dot {
                self.bump();
                if self.peek(0).kind == K::Ident {
                    let token = self.bump();
                    path.push(token.text(self.source).to_owned());
                } else {
                    break;
                }
            }
        }
        path
    }

    fn file_annotation(&mut self) -> bool {
        let target = self.peek(1);
        if self.peek(0).kind != K::At
            || !bare(self.source, target, "file")
            || self.peek(2).kind != K::Colon
        {
            return false;
        }
        self.bump();
        self.bump();
        self.bump();
        if self.peek(0).kind == K::LBracket {
            return self.group();
        }
        if self.peek(0).kind != K::Ident {
            return false;
        }
        loop {
            self.bump(); // annotation classifier segment
            if self.peek(0).kind == K::Lt && !self.group() {
                return false;
            }
            if self.peek(0).kind != K::Dot {
                break;
            }
            self.bump();
            if self.peek(0).kind != K::Ident {
                return false;
            }
        }
        if self.peek(0).kind == K::LParen {
            return self.group();
        }
        true
    }

    fn group(&mut self) -> bool {
        let close = |kind| match kind {
            K::LParen => Some(K::RParen),
            K::LBracket => Some(K::RBracket),
            K::LBrace => Some(K::RBrace),
            K::Lt => Some(K::Gt),
            _ => None,
        };
        let opening = self.bump().kind;
        let angles = opening == K::Lt;
        let mut stack = vec![close(opening).expect("a header group begins at an opener")];
        while let Some(&expected) = stack.last() {
            let token = self.bump();
            if token.kind == K::Eof {
                return false;
            }
            if token.kind == expected {
                stack.pop();
            } else if token.kind != K::Lt || angles {
                if let Some(end) = close(token.kind) {
                    stack.push(end);
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests;
