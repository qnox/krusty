//! kotlinc's IR measures offsets in UTF-16 code units of the text it reads, in which every line
//! break is a `\n`: it reads `\r\n` as `\n` and a lone `\r` as `\n`. krusty's spans are UTF-8 byte
//! offsets into the file's text as written.

use crate::diag::Span;

/// The text kotlinc reads from a file whose text is `text`.
pub(super) fn kotlinc_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The offset in [`kotlinc_text`], in UTF-16 units, of every UTF-8 byte offset of one file's text.
pub(super) struct Utf16Offsets(Vec<u32>);

impl Utf16Offsets {
    pub(super) fn new(text: &str) -> Self {
        let mut offsets = Vec::with_capacity(text.len() + 1);
        let mut units = 0u32;
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            // A byte inside a character is not a span boundary; it maps where the character starts.
            offsets.extend(std::iter::repeat_n(units, character.len_utf8()));
            // The `\r` of a `\r\n` is not in kotlinc's text, so it maps where its `\n` starts.
            if character != '\r' || characters.peek() != Some(&'\n') {
                units += character.len_utf16() as u32;
            }
        }
        offsets.push(units);
        Self(offsets)
    }

    pub(super) fn span(&self, span: Span) -> krusty_plugin_ir::Span {
        krusty_plugin_ir::Span {
            start: self.0[span.lo as usize],
            end: self.0[span.hi as usize],
        }
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
        assert_eq!(
            offsets.span(Span::new(6, 7)),
            krusty_plugin_ir::Span { start: 3, end: 4 }
        );
    }

    #[test]
    fn a_span_after_a_crlf_counts_it_as_one_line_break_and_a_lone_cr_as_one_too() {
        let text = "a\r\nb\rc";
        let offsets = Utf16Offsets::new(text);
        assert_eq!(kotlinc_text(text), "a\nb\nc");
        assert_eq!(
            offsets.span(Span::new(3, 4)),
            krusty_plugin_ir::Span { start: 2, end: 3 }
        );
        assert_eq!(
            offsets.span(Span::new(5, 6)),
            krusty_plugin_ir::Span { start: 4, end: 5 }
        );
    }
}
