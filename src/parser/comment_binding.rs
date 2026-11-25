//! Which comments belong to a declaration's source range.
//!
//! The lexer drops trivia, so a parser that records a declaration's whole range recovers the
//! comments around it from the gap between two tokens, which holds only whitespace and comments.
//! The rule is kotlinc's: a comment on the line that the previous token ends stays with the
//! previous declaration, the comments on the lines after it lead the next one, and the comments
//! that follow a declaration's closing `,` or `;` on its own line trail it.

/// Where a declaration whose first token starts at `declaration_lo` begins, given the end of the
/// token before it: the first comment on a later line than that token, else the token itself.
pub(super) fn leading_start(source: &str, previous_end: u32, declaration_lo: u32) -> u32 {
    let bytes = source.as_bytes();
    let end = (declaration_lo as usize).min(bytes.len());
    let mut at = previous_end as usize;
    let mut on_later_line = false;
    while at < end {
        match bytes[at] {
            b'\n' => {
                on_later_line = true;
                at += 1;
            }
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                if on_later_line {
                    return at as u32;
                }
                at = line_end(bytes, at);
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                if on_later_line {
                    return at as u32;
                }
                at = block_comment_end(bytes, at);
            }
            _ => at += 1,
        }
    }
    declaration_lo
}

/// Where a declaration whose last token ends at `last_token_hi` ends: through the `,` or `;`
/// that follows it (even past a line break or comments), then through the comments that start on
/// that line.
pub(super) fn trailing_end(source: &str, last_token_hi: u32) -> u32 {
    let bytes = source.as_bytes();
    let mut end = last_token_hi as usize;
    let mut at = skip_trivia(bytes, end, true);
    if matches!(bytes.get(at), Some(b',' | b';')) {
        at += 1;
        end = at;
    } else {
        at = end;
    }
    loop {
        let next = skip_trivia(bytes, at, false);
        match (bytes.get(next), bytes.get(next + 1)) {
            (Some(b'/'), Some(b'/')) => {
                at = line_end(bytes, next);
                end = at;
            }
            (Some(b'/'), Some(b'*')) => {
                at = block_comment_end(bytes, next);
                end = at;
            }
            _ => break,
        }
    }
    end as u32
}

/// The first offset at or after `at` that is not whitespace or, when `comments` is set, a comment.
/// Without `comments` it also stops at a line break.
fn skip_trivia(bytes: &[u8], mut at: usize, comments: bool) -> usize {
    while at < bytes.len() {
        match bytes[at] {
            b'\n' if !comments => break,
            b' ' | b'\t' | b'\r' | b'\n' => at += 1,
            b'/' if comments && bytes.get(at + 1) == Some(&b'/') => at = line_end(bytes, at),
            b'/' if comments && bytes.get(at + 1) == Some(&b'*') => {
                at = block_comment_end(bytes, at);
            }
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
