//! kotlinc's IR measures offsets in UTF-16 code units of the text it reads, in which every line
//! break is a `\n`: it reads `\r\n` as `\n` and a lone `\r` as `\n`. krusty's spans are UTF-8 byte
//! offsets into the file's text as written. A hosted plugin receives kotlinc's offsets, and a
//! native plugin that writes them into generated code (Compose's source information) computes
//! them here.

/// The text kotlinc reads from a file whose text is `text`.
pub(crate) fn kotlinc_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The offset in [`kotlinc_text`], in UTF-16 units, of every UTF-8 byte offset of one file's text.
pub(crate) struct Utf16Offsets {
    units: Vec<u32>,
    /// The offset in UTF-16 units where each line of kotlinc's text starts; line 0 at 0.
    line_starts: Vec<u32>,
}

impl Utf16Offsets {
    pub(crate) fn new(text: &str) -> Self {
        let mut offsets = Vec::with_capacity(text.len() + 1);
        let mut line_starts = vec![0];
        let mut units = 0u32;
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            // A byte inside a character is not a span boundary; it maps where the character starts.
            offsets.extend(std::iter::repeat_n(units, character.len_utf8()));
            // The `\r` of a `\r\n` is not in kotlinc's text, so it maps where its `\n` starts.
            let crlf = character == '\r' && characters.peek() == Some(&'\n');
            if !crlf {
                units += character.len_utf16() as u32;
                if matches!(character, '\n' | '\r') {
                    line_starts.push(units);
                }
            }
        }
        offsets.push(units);
        Self {
            units: offsets,
            line_starts,
        }
    }

    /// The UTF-16 offset in kotlinc's text of the UTF-8 byte offset `byte`.
    pub(crate) fn unit(&self, byte: u32) -> u32 {
        self.units[byte as usize]
    }

    /// kotlinc's 0-based line number (`IrFileEntry.getLineNumber`) of the UTF-8 byte offset `byte`.
    pub(crate) fn line(&self, byte: u32) -> u32 {
        let unit = self.unit(byte);
        (self.line_starts.partition_point(|&start| start <= unit) - 1) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_after_a_character_outside_the_bmp_counts_its_two_utf16_units() {
        let text = "é😀x";
        let offsets = Utf16Offsets::new(text);
        // `x` is at byte 6: two bytes of `é`, four of the emoji.
        assert_eq!((offsets.unit(6), offsets.unit(7)), (3, 4));
    }

    #[test]
    fn lines_count_every_kotlinc_line_break_once() {
        let text = "a\r\nb\rc\nd";
        let offsets = Utf16Offsets::new(text);
        assert_eq!(
            (0..text.len() as u32)
                .map(|byte| offsets.line(byte))
                .collect::<Vec<_>>(),
            vec![0, 0, 0, 1, 1, 2, 2, 3]
        );
    }

    #[test]
    fn a_span_after_a_crlf_counts_it_as_one_line_break_and_a_lone_cr_as_one_too() {
        let text = "a\r\nb\rc";
        let offsets = Utf16Offsets::new(text);
        assert_eq!(kotlinc_text(text), "a\nb\nc");
        assert_eq!((offsets.unit(3), offsets.unit(4)), (2, 3));
        assert_eq!((offsets.unit(5), offsets.unit(6)), (4, 5));
    }
}
