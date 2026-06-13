//! Token kinds for the krusty Kotlin subset.

use crate::diag::Span;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    // literals & names
    Ident,
    IntLit,    // 123
    LongLit,   // 123L
    DoubleLit, // 1.5
    StringLit, // "..."
    // keywords
    KwFun,
    KwClass,
    KwVal,
    KwVar,
    KwReturn,
    KwIf,
    KwElse,
    KwWhen,
    KwWhile,
    KwFor,
    KwIn,
    KwTrue,
    KwFalse,
    KwNull,
    KwPackage,
    KwImport,
    // punctuation / operators
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    Eq,       // =
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,     // ==
    NotEq,    // !=
    Lt,
    LtEq,
    Gt,
    GtEq,
    AndAnd,   // &&
    OrOr,     // ||
    Not,      // !
    Arrow,    // ->  (when arms, lambdas)
    DotDot,   // ..  (range)
    PlusEq,   // +=
    MinusEq,  // -=
    StarEq,   // *=
    SlashEq,  // /=
    PercentEq,// %=
    Question, // ?   (nullable types, ?. , ?:)
    At,       // @   (annotations)
    // trivia / control
    Newline,
    Eof,
    Unknown,
}

#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.span.lo as usize..self.span.hi as usize]
    }
}

/// Maps an identifier's text to a keyword kind, or `None` if it is a plain identifier.
/// Type names (Int, String, ...) are intentionally NOT keywords — they resolve later.
pub fn keyword(text: &str) -> Option<TokenKind> {
    Some(match text {
        "fun" => TokenKind::KwFun,
        "class" => TokenKind::KwClass,
        "val" => TokenKind::KwVal,
        "var" => TokenKind::KwVar,
        "return" => TokenKind::KwReturn,
        "if" => TokenKind::KwIf,
        "else" => TokenKind::KwElse,
        "when" => TokenKind::KwWhen,
        "while" => TokenKind::KwWhile,
        "for" => TokenKind::KwFor,
        "in" => TokenKind::KwIn,
        "true" => TokenKind::KwTrue,
        "false" => TokenKind::KwFalse,
        "null" => TokenKind::KwNull,
        "package" => TokenKind::KwPackage,
        "import" => TokenKind::KwImport,
        _ => return None,
    })
}
