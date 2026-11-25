//! Which comments belong to a declaration's source range.
//!
//! The lexer drops trivia, so a parser that records a declaration's whole range recovers the
//! comments around it from the gap between two tokens, which holds only whitespace and comments.
//! The rules are those of kotlinc's `PrecedingCommentsBinder` and `TrailingCommentsBinder`, which
//! it applies to enum entries.

#[derive(Clone, Copy, Eq, PartialEq)]
enum Trivia {
    Whitespace { line_breaks: u32 },
    LineComment,
    BlockComment,
    DocComment,
}

struct Piece {
    kind: Trivia,
    lo: usize,
}

/// Splits `source[from..to]`, which holds only whitespace and comments, into its tokens.
fn pieces(source: &str, from: u32, to: u32) -> Vec<Piece> {
    let bytes = source.as_bytes();
    let end = (to as usize).min(bytes.len());
    let mut at = from as usize;
    let mut pieces = Vec::new();
    while at < end {
        let lo = at;
        let kind = match (bytes[at], bytes.get(at + 1)) {
            (b'/', Some(b'/')) => {
                at = line_end(bytes, at);
                Trivia::LineComment
            }
            (b'/', Some(b'*')) => {
                let doc = bytes.get(at + 2) == Some(&b'*') && bytes.get(at + 3) != Some(&b'/');
                at = block_comment_end(bytes, at);
                if doc {
                    Trivia::DocComment
                } else {
                    Trivia::BlockComment
                }
            }
            _ => {
                let mut line_breaks = 0;
                while at < end
                    && !(bytes[at] == b'/' && matches!(bytes.get(at + 1), Some(b'/' | b'*')))
                {
                    line_breaks += u32::from(bytes[at] == b'\n');
                    at += 1;
                }
                Trivia::Whitespace { line_breaks }
            }
        };
        pieces.push(Piece { kind, lo });
    }
    pieces
}

/// Where a declaration whose first token starts at `declaration_lo` begins, given where the gap
/// before it starts: at the last KDoc comment of the gap if there is one; otherwise at the
/// earliest comment that follows a line break and is not cut off from the declaration by a blank
/// line. A comment at the very start of the gap also binds.
pub(super) fn leading_start(source: &str, gap_start: u32, declaration_lo: u32) -> u32 {
    let gap = pieces(source, gap_start, declaration_lo);
    if let Some(doc) = gap
        .iter()
        .rev()
        .find(|piece| piece.kind == Trivia::DocComment)
    {
        return doc.lo as u32;
    }
    let mut start = declaration_lo;
    for index in (0..gap.len()).rev() {
        match gap[index].kind {
            Trivia::Whitespace { line_breaks } if line_breaks > 1 => break,
            Trivia::LineComment | Trivia::BlockComment => {
                let follows_line_break = index == 0
                    || matches!(gap[index - 1].kind, Trivia::Whitespace { line_breaks } if line_breaks > 0);
                if follows_line_break {
                    start = gap[index].lo as u32;
                }
            }
            _ => {}
        }
    }
    start
}

/// Where a declaration whose last token ends at `last_token_hi` ends: through the `,` and then
/// the `;` that follow it, even past line breaks and comments, then through the plain comments
/// that follow on the same line.
pub(super) fn trailing_end(source: &str, last_token_hi: u32) -> u32 {
    let bytes = source.as_bytes();
    let mut end = last_token_hi as usize;
    for separator in *b",;" {
        let at = skip_trivia(bytes, end);
        if bytes.get(at) == Some(&separator) {
            end = at + 1;
        }
    }
    let rest = pieces(source, end as u32, skip_trivia(bytes, end) as u32);
    for piece in &rest {
        match piece.kind {
            Trivia::Whitespace { line_breaks: 0 } => {}
            Trivia::LineComment | Trivia::BlockComment => {
                end = comment_end(bytes, piece);
            }
            _ => break,
        }
    }
    end as u32
}

fn comment_end(bytes: &[u8], piece: &Piece) -> usize {
    if piece.kind == Trivia::LineComment {
        line_end(bytes, piece.lo)
    } else {
        block_comment_end(bytes, piece.lo)
    }
}

/// The first offset at or after `at` that is not whitespace or part of a comment.
fn skip_trivia(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() {
        match (bytes[at], bytes.get(at + 1)) {
            (b' ' | b'\t' | b'\r' | b'\n', _) => at += 1,
            (b'/', Some(b'/')) => at = line_end(bytes, at),
            (b'/', Some(b'*')) => at = block_comment_end(bytes, at),
            _ => break,
        }
    }
    at
}

fn line_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| from + offset)
}

/// The offset after the `*/` closing the block comment that starts at `from`; Kotlin block
/// comments nest.
fn block_comment_end(bytes: &[u8], from: usize) -> usize {
    let mut depth = 0usize;
    let mut at = from;
    while at < bytes.len() {
        if bytes[at] == b'/' && bytes.get(at + 1) == Some(&b'*') {
            depth += 1;
            at += 2;
        } else if bytes[at] == b'*' && bytes.get(at + 1) == Some(&b'/') {
            depth -= 1;
            at += 2;
            if depth == 0 {
                return at;
            }
        } else {
            at += 1;
        }
    }
    bytes.len()
}
