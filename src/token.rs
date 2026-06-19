//! Token kinds for the krusty Kotlin subset.

use crate::diag::Span;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    // literals & names
    Ident,
    IntLit,    // 123
    LongLit,   // 123L
    UIntLit,   // 123u / 0xFFu
    ULongLit,  // 123uL
    DoubleLit, // 1.5
    FloatLit,  // 1.5f / 1f
    StringLit, // "..." (no interpolation)
    CharLit,   // 'x'
    // string templates: TemplateStart StrChunk (Dollar Ident | Dollar LBrace expr RBrace | StrChunk)* TemplateEnd
    TemplateStart,
    TemplateEnd,
    StrChunk, // a literal text piece of a template (text() is the raw chunk)
    Dollar,   // `$` before an interpolation
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
    KwDo,
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
    Eq, // =
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,  // ==
    NotEq, // !=
    RefEq, // ===
    RefNe, // !==
    Lt,
    LtEq,
    Gt,
    GtEq,
    AndAnd,     // &&
    OrOr,       // ||
    Not,        // !
    Arrow,      // ->  (when arms, lambdas)
    DotDot,     // ..  (range)
    DotDotLt,   // ..< (rangeUntil)
    PlusPlus,   // ++
    MinusMinus, // --
    PlusEq,     // +=
    MinusEq,    // -=
    StarEq,     // *=
    SlashEq,    // /=
    PercentEq,  // %=
    ColonColon, // ::  (callable references, class literals)
    Question,   // ?   (nullable types, ?. , ?:)
    At,         // @   (annotations)
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
        "do" => TokenKind::KwDo,
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
