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
    // like TemplateStart but for a raw (triple-quoted) template: its StrChunk pieces are verbatim
    // (no escape processing), so the parser must not run `unescape_chunk` on them.
    RawTemplateStart,
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
    Amp,        // &  (definitely-non-null intersection `T & Any`)
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

/// Why the CONTENT between a `Char` literal's quotes is not one Kotlin `Char`.
///
/// This contract lives with the token rather than in either consumer: the lexer needs the exact
/// diagnostic category, while the parser needs the decoded UTF-16 unit. Keeping both answers in
/// one function prevents their accepted escape sets from drifting apart. In particular, Kotlin
/// has no `\0` escape even though the still-separate string decoder currently accepts one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CharLiteralError {
    Empty,
    Incorrect,
    UnsupportedEscape,
    TooManyCharacters,
}

impl CharLiteralError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Empty => "empty character literal",
            Self::Incorrect => "incorrect character literal",
            Self::UnsupportedEscape => "unsupported escape sequence",
            Self::TooManyCharacters => "too many characters in a character literal",
        }
    }
}

/// Validate and decode the CONTENT between a `Char` literal's quotes.
///
/// A Kotlin `Char` is exactly one UTF-16 code unit. Raw source is valid UTF-8, so one raw scalar is
/// acceptable only when `len_utf16() == 1`; an astral scalar occupies two units and is rejected. A
/// `\uXXXX` escape is different: its four digits denote a unit directly, including an isolated
/// surrogate such as D83D, which cannot be represented as a Rust `char` but is legal Kotlin.
///
/// kotlinc classifies malformed literals by their lexical shape. CR/LF are forbidden by the
/// grammar before element counting; content beginning with `\` must be exactly one supported
/// escape; other content must be exactly one BMP scalar. Returning the decoded unit together with
/// that classification gives every compiler path the same syntax authority.
pub(crate) fn decode_char_literal_content(inner: &str) -> Result<u16, CharLiteralError> {
    if inner.contains(['\n', '\r']) {
        return Err(CharLiteralError::Incorrect);
    }
    if inner.is_empty() {
        return Err(CharLiteralError::Empty);
    }
    if inner.starts_with('\\') {
        let unit = match inner.as_bytes() {
            b"\\n" => '\n' as u16,
            b"\\t" => '\t' as u16,
            b"\\r" => '\r' as u16,
            b"\\b" => 0x0008,
            b"\\\\" => '\\' as u16,
            b"\\'" => '\'' as u16,
            b"\\\"" => '"' as u16,
            b"\\$" => '$' as u16,
            bytes if bytes.len() == 6 && bytes.starts_with(b"\\u") => {
                u16::from_str_radix(&inner[2..], 16)
                    .map_err(|_| CharLiteralError::UnsupportedEscape)?
            }
            _ => return Err(CharLiteralError::UnsupportedEscape),
        };
        return Ok(unit);
    }

    let mut chars = inner.chars();
    let value = chars.next().expect("non-empty literal content");
    if chars.next().is_some() || value.len_utf16() != 1 {
        return Err(CharLiteralError::TooManyCharacters);
    }
    Ok(value as u32 as u16)
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
