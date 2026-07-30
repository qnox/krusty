pub(super) fn utf8_char_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first < 0xE0 {
        2
    } else if first < 0xF0 {
        3
    } else {
        4
    }
}

pub(super) fn normalized_scan_end(bytes: &[u8], end: usize) -> usize {
    let mut end = end.min(bytes.len());
    while end > 0 && end < bytes.len() && bytes[end] & 0b1100_0000 == 0b1000_0000 {
        end -= 1;
    }
    end
}

pub(super) fn bounded_utf8_advance(bytes: &[u8], index: usize, end: usize) -> usize {
    let end = normalized_scan_end(bytes, end);
    if index >= end {
        return end;
    }
    index.saturating_add(utf8_char_len(bytes[index])).min(end)
}

fn token_within(bytes: &[u8], index: usize, end: usize, token: &[u8]) -> bool {
    index
        .checked_add(token.len())
        .is_some_and(|hi| hi <= end && bytes.get(index..hi) == Some(token))
}

pub(super) fn skip_line_comment(bytes: &[u8], mut index: usize, end: usize) -> usize {
    let end = normalized_scan_end(bytes, end);
    if index >= end {
        return end;
    }
    index = index.saturating_add(2).min(end);
    while index < end && !matches!(bytes.get(index), Some(b'\n' | b'\r')) {
        index += 1;
    }
    index
}

pub(super) fn skip_block_comment(bytes: &[u8], mut index: usize, end: usize) -> usize {
    let end = normalized_scan_end(bytes, end);
    if index >= end {
        return end;
    }
    let mut depth = 0usize;
    while index < end {
        if token_within(bytes, index, end, b"/*") {
            depth += 1;
            index += 2;
        } else if token_within(bytes, index, end, b"*/") {
            depth = depth.saturating_sub(1);
            index = index.saturating_add(2).min(end);
            if depth == 0 {
                break;
            }
        } else {
            index = bounded_utf8_advance(bytes, index, end);
        }
    }
    index
}

pub(super) fn skip_quoted(bytes: &[u8], start: usize, end: usize) -> usize {
    let end = normalized_scan_end(bytes, end);
    if start >= end {
        return end;
    }
    let Some(&quote) = bytes.get(start) else {
        return end;
    };
    let triple = quote == b'"' && token_within(bytes, start, end, b"\"\"\"");
    let mut index = start.saturating_add(if triple { 3 } else { 1 }).min(end);
    let mut escaped = false;
    while index < end {
        if triple && token_within(bytes, index, end, b"\"\"\"") {
            return index + 3;
        }
        let byte = bytes[index];
        if !triple && !escaped && byte == quote {
            return index + 1;
        }
        escaped = !triple && quote != b'`' && !escaped && byte == b'\\';
        index = bounded_utf8_advance(bytes, index, end);
    }
    end
}

pub(super) fn skip_trivia(bytes: &[u8], mut index: usize, end: usize) -> usize {
    let end = normalized_scan_end(bytes, end);
    if index >= end {
        return end;
    }
    loop {
        while index < end && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= end {
            return end;
        }
        if token_within(bytes, index, end, b"//") {
            index = skip_line_comment(bytes, index, end);
        } else if token_within(bytes, index, end, b"/*") {
            index = skip_block_comment(bytes, index, end);
        } else {
            return index;
        }
    }
}

pub(super) fn matching_delimiter(
    bytes: &[u8],
    open: usize,
    end: usize,
    left: u8,
    right: u8,
) -> Option<usize> {
    let end = normalized_scan_end(bytes, end);
    if open >= end || bytes[open] != left {
        return None;
    }
    let mut depth = 0usize;
    let mut index = open;
    while index < end {
        match bytes[index] {
            b'/' if token_within(bytes, index, end, b"//") => {
                index = skip_line_comment(bytes, index, end);
            }
            b'/' if token_within(bytes, index, end, b"/*") => {
                index = skip_block_comment(bytes, index, end);
            }
            b'"' | b'\'' | b'`' => index = skip_quoted(bytes, index, end),
            byte if byte == left => {
                depth += 1;
                index += 1;
            }
            byte if byte == right => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
                index += 1;
            }
            _ => index = bounded_utf8_advance(bytes, index, end),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimiter_scan_respects_comments_strings_and_bounds() {
        let source = "(/* nested /* ) */ end */ \"\"\" ) \"\"\" `)` ')') tail";
        assert_eq!(
            matching_delimiter(source.as_bytes(), 0, source.len(), b'(', b')'),
            source.rfind(')')
        );
        assert_eq!(
            matching_delimiter(source.as_bytes(), 0, source.len() - 6, b'(', b')'),
            None
        );
    }

    #[test]
    fn scanners_respect_utf8_bounds() {
        let quoted = "\"\u{00e9}\u{4e2d}\u{1f600}\" tail";
        let line = "// \u{00e9}\u{4e2d}\u{1f600}\nnext";
        let block = "/* \u{00e9}\u{4e2d}\u{1f600} */ next";
        let trivia = " \t/* \u{00e9}\u{4e2d}\u{1f600} */ value";

        for (source, scan) in [
            (quoted, skip_quoted as fn(&[u8], usize, usize) -> usize),
            (line, skip_line_comment),
            (block, skip_block_comment),
            (trivia, skip_trivia),
        ] {
            for requested_end in 0..=source.len() {
                let end = normalized_scan_end(source.as_bytes(), requested_end);
                for start in (0..=source.len()).filter(|start| source.is_char_boundary(*start)) {
                    let result = scan(source.as_bytes(), start, requested_end);
                    assert!(end <= requested_end);
                    assert!(source.is_char_boundary(end));
                    assert!(result <= end, "{result} exceeded {end} for {source:?}");
                    if start < end {
                        assert!(result >= start);
                    } else {
                        assert_eq!(result, end);
                    }
                    assert!(source.is_char_boundary(result));
                }
            }
        }

        let delimited = "(\u{00e9}(\u{4e2d})\u{1f600}) tail";
        for requested_end in 0..=delimited.len() {
            let end = normalized_scan_end(delimited.as_bytes(), requested_end);
            for open in (0..=delimited.len()).filter(|open| delimited.is_char_boundary(*open)) {
                let result =
                    matching_delimiter(delimited.as_bytes(), open, requested_end, b'(', b')');
                assert!(result
                    .is_none_or(|result| { result < end && delimited.is_char_boundary(result) }));
            }
        }
    }

    #[test]
    fn line_comments_stop_at_either_kotlin_line_separator() {
        assert_eq!(skip_line_comment(b"// comment\rnext", 0, 15), 10);
        assert_eq!(skip_line_comment(b"// comment\nnext", 0, 15), 10);
    }
}
