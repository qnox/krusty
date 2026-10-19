//! kotlinc's IR measures offsets in UTF-16 code units; krusty's spans are UTF-8 byte offsets.

use crate::diag::Span;

/// The UTF-16 offset of every UTF-8 byte offset of one file's text.
pub(super) struct Utf16Offsets(Vec<u32>);

impl Utf16Offsets {
    pub(super) fn new(text: &str) -> Self {
        let mut offsets = Vec::with_capacity(text.len() + 1);
        let mut units = 0u32;
        for character in text.chars() {
            // A byte inside a character is not a span boundary; it maps where the character starts.
            offsets.extend(std::iter::repeat_n(units, character.len_utf8()));
            units += character.len_utf16() as u32;
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
}
