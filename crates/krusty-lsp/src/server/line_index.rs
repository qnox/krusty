//! Byte offsets where each LSP line starts.
//!
//! Cursor queries translate one UTF-16 position. Scanning from byte 0 does that correctly, and it
//! repeats the scan for every hover, completion, and navigation request on the same buffer. The
//! index is built once per text and then resolves a position from the start of its line.

/// An LSP position. `character` counts UTF-16 code units, matching the protocol rather than bytes
/// or Unicode scalars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Position {
    pub(crate) line: u32,
    pub(crate) character: u32,
}

impl Position {
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// Translate an LSP UTF-16 position into a source byte offset.
pub fn position_to_byte_offset(text: &str, target: Position) -> Option<u32> {
    let mut scan_budget = usize::MAX;
    position_to_byte_offset_with_budget(text, target, &mut scan_budget)
}

pub(super) fn position_to_byte_offset_with_budget(
    text: &str,
    target: Position,
    scan_budget: &mut usize,
) -> Option<u32> {
    let mut line = 0u32;
    let mut character = 0u32;
    let mut previous_was_cr = false;
    for (byte, ch) in text.char_indices() {
        if !(previous_was_cr && ch == '\n') && line == target.line && character == target.character
        {
            return u32::try_from(byte).ok();
        }
        *scan_budget = scan_budget.checked_sub(ch.len_utf8())?;
        match ch {
            '\r' => {
                line = line.checked_add(1)?;
                character = 0;
                previous_was_cr = true;
            }
            '\n' => {
                if !previous_was_cr {
                    line = line.checked_add(1)?;
                }
                character = 0;
                previous_was_cr = false;
            }
            _ => {
                character = character.checked_add(ch.len_utf16() as u32)?;
                previous_was_cr = false;
            }
        }
        if line > target.line || (line == target.line && character > target.character) {
            return None;
        }
    }
    (line == target.line && character == target.character)
        .then(|| u32::try_from(text.len()).ok())
        .flatten()
}

/// Translate a compiler byte offset into the UTF-16 code-unit position required by LSP.
pub fn byte_offset_to_position(text: &str, offset: usize) -> Position {
    let limit = offset.min(text.len());
    let mut line = 0u32;
    let mut character = 0u32;
    let mut previous_was_cr = false;

    for (byte, ch) in text.char_indices() {
        if byte >= limit || byte + ch.len_utf8() > limit {
            break;
        }
        match ch {
            '\r' => {
                line = line.saturating_add(1);
                character = 0;
                previous_was_cr = true;
            }
            '\n' => {
                if !previous_was_cr {
                    line = line.saturating_add(1);
                }
                character = 0;
                previous_was_cr = false;
            }
            _ => {
                character = character.saturating_add(ch.len_utf16() as u32);
                previous_was_cr = false;
            }
        }
    }
    Position::new(line, character)
}

#[derive(Clone, Debug)]
pub(crate) struct LineIndex {
    /// Byte offset of LSP position `(line, 0)`, including the empty line after a trailing break.
    starts: Vec<u32>,
}

impl LineIndex {
    pub(crate) fn new(text: &str) -> Self {
        let mut starts = Vec::new();
        starts.push(0);
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'\r' => {
                    index += 1;
                    if bytes.get(index) == Some(&b'\n') {
                        index += 1;
                    }
                    starts.push(u32::try_from(index).unwrap_or(u32::MAX));
                }
                b'\n' => {
                    index += 1;
                    starts.push(u32::try_from(index).unwrap_or(u32::MAX));
                }
                byte => {
                    index += utf8_char_len(byte);
                }
            }
        }
        Self { starts }
    }

    pub(crate) fn position_to_offset(&self, text: &str, line: u32, character: u32) -> Option<u32> {
        let start = *self.starts.get(usize::try_from(line).ok()?)? as usize;
        if start > text.len() {
            return None;
        }
        let mut current_line = line;
        let mut current_character = 0u32;
        let mut previous_was_cr = false;
        for (relative, ch) in text[start..].char_indices() {
            let byte = start + relative;
            if !(previous_was_cr && ch == '\n')
                && current_line == line
                && current_character == character
            {
                return u32::try_from(byte).ok();
            }
            match ch {
                '\r' => {
                    current_line = current_line.checked_add(1)?;
                    current_character = 0;
                    previous_was_cr = true;
                }
                '\n' => {
                    if !previous_was_cr {
                        current_line = current_line.checked_add(1)?;
                    }
                    current_character = 0;
                    previous_was_cr = false;
                }
                _ => {
                    current_character = current_character.checked_add(ch.len_utf16() as u32)?;
                    previous_was_cr = false;
                }
            }
            if current_line > line || (current_line == line && current_character > character) {
                return None;
            }
        }
        (current_line == line && current_character == character)
            .then(|| u32::try_from(text.len()).ok())
            .flatten()
    }

    pub(crate) fn offset_to_position(&self, text: &str, offset: usize) -> (u32, u32) {
        let limit = offset.min(text.len());
        let line = self.line_at(limit);
        let start = self.starts[line] as usize;
        let mut current_line = u32::try_from(line).unwrap_or(u32::MAX);
        let mut character = 0u32;
        let mut previous_was_cr = false;
        for (relative, ch) in text[start..].char_indices() {
            let byte = start + relative;
            if byte >= limit || byte + ch.len_utf8() > limit {
                break;
            }
            match ch {
                '\r' => {
                    current_line = current_line.saturating_add(1);
                    character = 0;
                    previous_was_cr = true;
                }
                '\n' => {
                    if !previous_was_cr {
                        current_line = current_line.saturating_add(1);
                    }
                    character = 0;
                    previous_was_cr = false;
                }
                _ => {
                    character = character.saturating_add(ch.len_utf16() as u32);
                    previous_was_cr = false;
                }
            }
        }
        (current_line, character)
    }

    fn line_at(&self, offset: usize) -> usize {
        let offset = u32::try_from(offset).unwrap_or(u32::MAX);
        match self.starts.binary_search(&offset) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        }
    }
}

fn utf8_char_len(byte: u8) -> usize {
    match byte {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_index_matches_scan(text: &str) {
        let index = LineIndex::new(text);
        for offset in 0..=text.len() {
            let scanned = byte_offset_to_position(text, offset);
            let (line, character) = index.offset_to_position(text, offset);
            assert_eq!(
                Position::new(line, character),
                scanned,
                "offset {offset} in {text:?}"
            );
            assert_eq!(
                index.position_to_offset(text, scanned.line, scanned.character),
                position_to_byte_offset(text, scanned),
                "round trip of offset {offset} in {text:?}"
            );
        }
        let lines = index.starts.len() as u32;
        for line in 0..lines + 1 {
            for character in 0..8 {
                let position = Position::new(line, character);
                assert_eq!(
                    index.position_to_offset(text, line, character),
                    position_to_byte_offset(text, position),
                    "position {line}:{character} in {text:?}"
                );
            }
        }
    }

    #[test]
    fn indexed_positions_match_the_linear_scan() {
        let texts = [
            "",
            "a",
            "\n",
            "\r",
            "\r\n",
            "a\nb",
            "a\rb",
            "a\r\nb",
            "a\n\nb",
            "a\r\n\r\nb",
            "a😀\r\nβz",
            "\n\n",
            "line\r\n",
        ];
        for text in texts {
            assert_index_matches_scan(text);
        }

        let mut many_lines = String::new();
        for line in 0..1000 {
            many_lines.push_str(&format!("line {line} 😀\n"));
        }
        let index = LineIndex::new(&many_lines);
        let last = byte_offset_to_position(&many_lines, many_lines.len());
        let (line, character) = index.offset_to_position(&many_lines, many_lines.len());
        assert_eq!(Position::new(line, character), last);
        assert_eq!(
            index.position_to_offset(&many_lines, last.line, 0),
            position_to_byte_offset(&many_lines, Position::new(last.line, 0))
        );
    }
}
