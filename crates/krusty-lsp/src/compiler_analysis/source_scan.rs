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

pub(super) fn skip_line_comment(bytes: &[u8], mut index: usize, end: usize) -> usize {
    index = index.saturating_add(2);
    while index < end && !matches!(bytes.get(index), Some(b'\n' | b'\r')) {
        index += 1;
    }
    index
}

pub(super) fn skip_block_comment(bytes: &[u8], mut index: usize, end: usize) -> usize {
    let end = end.min(bytes.len());
    let mut depth = 0usize;
    while index < end {
        if bytes.get(index..index.saturating_add(2)) == Some(b"/*") {
            depth += 1;
            index += 2;
        } else if bytes.get(index..index.saturating_add(2)) == Some(b"*/") {
            depth = depth.saturating_sub(1);
            index += 2;
            if depth == 0 {
                break;
            }
        } else {
            index += utf8_char_len(bytes[index]);
        }
    }
    index
}

pub(super) fn skip_quoted(bytes: &[u8], start: usize, end: usize) -> usize {
    let end = end.min(bytes.len());
    let Some(&quote) = bytes.get(start) else {
        return end;
    };
    let triple = quote == b'"' && bytes.get(start..start.saturating_add(3)) == Some(b"\"\"\"");
    let mut index = start.saturating_add(if triple { 3 } else { 1 });
    let mut escaped = false;
    while index < end {
        if triple && bytes.get(index..index.saturating_add(3)) == Some(b"\"\"\"") {
            return (index + 3).min(end);
        }
        let byte = bytes[index];
        if !triple && !escaped && byte == quote {
            return index + 1;
        }
        escaped = !triple && quote != b'`' && !escaped && byte == b'\\';
        index += utf8_char_len(byte);
    }
    end
}

pub(super) fn skip_trivia(bytes: &[u8], mut index: usize, end: usize) -> usize {
    let end = end.min(bytes.len());
    loop {
        while index < end && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if bytes.get(index..index.saturating_add(2)) == Some(b"//") {
            index = skip_line_comment(bytes, index, end);
        } else if bytes.get(index..index.saturating_add(2)) == Some(b"/*") {
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
    let end = end.min(bytes.len());
    if bytes.get(open) != Some(&left) || open >= end {
        return None;
    }
    let mut depth = 0usize;
    let mut index = open;
    while index < end {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index, end);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
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
            _ => index += utf8_char_len(bytes[index]),
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
    fn line_comments_stop_at_either_kotlin_line_separator() {
        assert_eq!(skip_line_comment(b"// comment\rnext", 0, 15), 10);
        assert_eq!(skip_line_comment(b"// comment\nnext", 0, 15), 10);
    }
}
