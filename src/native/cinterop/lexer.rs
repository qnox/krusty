//! C preprocessing tokens (C11 §6.4), with the flags the preprocessor needs: whether a token
//! starts a line (a directive can only begin there) and whether space precedes it (a function-like
//! macro's name must be followed directly by `(` in its definition, and `#` stringizes spacing).

use std::rc::Rc;

/// What kind of preprocessing token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Identifier,
    /// A preprocessing number: any `[.]?digit (letter|digit|.|e+|e-|p+|p-)*` run.
    Number,
    CharLiteral,
    StringLiteral,
    Punctuator,
    /// A character no other kind takes (a stray `@` or `\`); kept so a directive can name it.
    Other,
    /// The end of an included file: the preprocessor pops its include stack here.
    FileEnd,
}

/// Where a token came from, for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Location {
    pub(super) file: Rc<str>,
    pub(super) line: u32,
}

#[derive(Clone, Debug)]
pub(super) struct Token {
    pub(super) kind: Kind,
    pub(super) text: Rc<str>,
    pub(super) at_line_start: bool,
    pub(super) space_before: bool,
    pub(super) location: Rc<Location>,
    /// Macros whose expansion produced this token and may not expand it again (C11 §6.10.3.4).
    pub(super) hide: Rc<[Rc<str>]>,
}

impl Token {
    pub(super) fn is(&self, text: &str) -> bool {
        &*self.text == text && matches!(self.kind, Kind::Punctuator | Kind::Identifier)
    }

    pub(super) fn is_hidden(&self, name: &str) -> bool {
        self.hide.iter().any(|hidden| &**hidden == name)
    }
}

/// Every punctuator, longest first, so the lexer takes the longest match (C11 §6.4p4).
const PUNCTUATORS: &[&str] = &[
    "%:%:", "...", "<<=", ">>=", "->", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||",
    "*=", "/=", "%=", "+=", "-=", "&=", "^=", "|=", "##", "<:", ":>", "<%", "%>", "%:", "[", "]",
    "(", ")", "{", "}", ".", "&", "*", "+", "-", "~", "!", "/", "%", "<", ">", "^", "|", "?", ":",
    ";", "=", ",", "#",
];

/// A lexing failure, with where it happened.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct LexError {
    pub(super) file: String,
    pub(super) line: u32,
    pub(super) message: String,
}

/// Remove backslash-newline splices (translation phase 2), keeping a map from each output line
/// back to its physical line so diagnostics name the line a user sees.
fn splice(source: &str) -> (Vec<u8>, Vec<u32>) {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut lines = vec![1u32];
    let mut physical = 1u32;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\\' {
            let mut next = index + 1;
            if bytes.get(next) == Some(&b'\r') {
                next += 1;
            }
            if bytes.get(next) == Some(&b'\n') {
                physical += 1;
                index = next + 1;
                continue;
            }
        }
        if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            index += 1;
            continue;
        }
        out.push(byte);
        if byte == b'\n' {
            physical += 1;
            lines.push(physical);
        }
        index += 1;
    }
    (out, lines)
}

/// Tokenize one source file.
pub(super) fn tokenize(file: &str, source: &str) -> Result<Vec<Token>, LexError> {
    let (bytes, lines) = splice(source);
    let file_name: Rc<str> = Rc::from(file);
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 0usize;
    let mut at_line_start = true;
    let mut space_before = false;
    let no_hide: Rc<[Rc<str>]> = Rc::from(Vec::new());
    let mut location = Rc::new(Location {
        file: file_name.clone(),
        line: lines[0],
    });
    let error = |line: usize, message: String| LexError {
        file: file.to_string(),
        line: lines[line.min(lines.len() - 1)],
        message,
    };
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' {
            line += 1;
            at_line_start = true;
            space_before = false;
            index += 1;
            location = Rc::new(Location {
                file: file_name.clone(),
                line: lines[line.min(lines.len() - 1)],
            });
            continue;
        }
        if matches!(byte, b' ' | b'\t' | b'\x0b' | b'\x0c' | b'\r') {
            space_before = true;
            index += 1;
            continue;
        }
        if bytes[index..].starts_with(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            space_before = true;
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            let Some(end) = find(&bytes[index + 2..], b"*/") else {
                return Err(error(line, "unterminated comment".to_string()));
            };
            let comment = &bytes[index..index + 2 + end + 2];
            let newlines = comment.iter().filter(|&&byte| byte == b'\n').count();
            if newlines > 0 {
                line += newlines;
                location = Rc::new(Location {
                    file: file_name.clone(),
                    line: lines[line.min(lines.len() - 1)],
                });
            }
            index += comment.len();
            space_before = true;
            continue;
        }
        let start = index;
        let kind = if byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit))
        {
            index += 1;
            while index < bytes.len() {
                let current = bytes[index];
                if matches!(current, b'e' | b'E' | b'p' | b'P')
                    && matches!(bytes.get(index + 1), Some(b'+' | b'-'))
                {
                    index += 2;
                } else if current.is_ascii_alphanumeric() || current == b'.' || current == b'_' {
                    index += 1;
                } else if current == b'\''
                    && bytes.get(index + 1).is_some_and(u8::is_ascii_alphanumeric)
                {
                    // C23 digit separators, which newer headers may use.
                    index += 1;
                } else {
                    break;
                }
            }
            Kind::Number
        } else if let Some(quote) = literal_start(&bytes[index..]) {
            // An encoding prefix (`L`, `u`, `U`, `u8`) is part of the literal.
            index += quote.0;
            let delimiter = quote.1;
            index += 1;
            loop {
                match bytes.get(index) {
                    None | Some(b'\n') => {
                        return Err(error(line, "unterminated literal".to_string()));
                    }
                    Some(b'\\') => index += 2,
                    Some(&current) if current == delimiter => {
                        index += 1;
                        break;
                    }
                    Some(_) => index += 1,
                }
            }
            if delimiter == b'"' {
                Kind::StringLiteral
            } else {
                Kind::CharLiteral
            }
        } else if byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$' || byte >= 0x80 {
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric()
                    || bytes[index] == b'_'
                    || bytes[index] == b'$'
                    || bytes[index] >= 0x80)
            {
                index += 1;
            }
            Kind::Identifier
        } else if let Some(punctuator) = PUNCTUATORS
            .iter()
            .find(|punctuator| bytes[index..].starts_with(punctuator.as_bytes()))
        {
            index += punctuator.len();
            Kind::Punctuator
        } else {
            index += 1;
            Kind::Other
        };
        let raw = String::from_utf8_lossy(&bytes[start..index]);
        // Digraphs are the punctuators they spell (C11 §6.4.6p3).
        let text: &str = match &*raw {
            "<:" => "[",
            ":>" => "]",
            "<%" => "{",
            "%>" => "}",
            "%:" => "#",
            "%:%:" => "##",
            other => other,
        };
        tokens.push(Token {
            kind,
            text: Rc::from(text),
            at_line_start,
            space_before,
            location: location.clone(),
            hide: no_hide.clone(),
        });
        at_line_start = false;
        space_before = false;
    }
    Ok(tokens)
}

/// When `bytes` starts a character or string literal: the encoding prefix's length and the quote.
fn literal_start(bytes: &[u8]) -> Option<(usize, u8)> {
    for prefix in ["u8", "u", "U", "L", ""] {
        if let Some(rest) = bytes.strip_prefix(prefix.as_bytes()) {
            if let Some(&quote) = rest.first() {
                if quote == b'"' || quote == b'\'' {
                    return Some((prefix.len(), quote));
                }
            }
        }
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(source: &str) -> Vec<String> {
        tokenize("t.h", source)
            .expect("lexes")
            .iter()
            .map(|token| token.text.to_string())
            .collect()
    }

    #[test]
    fn tokens_take_the_longest_punctuator_and_keep_numbers_whole() {
        assert_eq!(
            texts("a>>=b...c->d 0x1fULL 1.5e+3f .5 'x' L\"s\\\"\""),
            [
                "a",
                ">>=",
                "b",
                "...",
                "c",
                "->",
                "d",
                "0x1fULL",
                "1.5e+3f",
                ".5",
                "'x'",
                "L\"s\\\"\""
            ]
        );
    }

    #[test]
    fn comments_and_splices_vanish_but_lines_are_kept() {
        let tokens = tokenize("t.h", "#define A \\\n  1 /* x\n y */ 2\nB // c\n").expect("lexes");
        let summary: Vec<(String, u32, bool, bool)> = tokens
            .iter()
            .map(|token| {
                (
                    token.text.to_string(),
                    token.location.line,
                    token.at_line_start,
                    token.space_before,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("#".into(), 1, true, false),
                ("define".into(), 1, false, false),
                ("A".into(), 1, false, true),
                ("1".into(), 1, false, true),
                ("2".into(), 3, false, true),
                ("B".into(), 4, true, false),
            ]
        );
    }

    #[test]
    fn an_unterminated_comment_is_an_error_naming_its_line() {
        assert_eq!(
            tokenize("t.h", "int a;\n/* open").expect_err("fails"),
            LexError {
                file: "t.h".to_string(),
                line: 2,
                message: "unterminated comment".to_string(),
            }
        );
    }
}
