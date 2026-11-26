//! Where a source file's lines start. kotlinc reads a lone `\r` as `\n` and `\r\n` as one `\n`, so
//! every line number krusty reports for a file (diagnostics, the line number table, the SMAP) is
//! counted by this one rule. Positions inside a line are bytes.

/// The byte offset where each line starts; line 1 starts at 0.
pub(crate) fn line_starts(text: &str) -> Vec<u32> {
    let bytes = text.as_bytes();
    let mut starts = vec![0u32];
    for (index, &byte) in bytes.iter().enumerate() {
        let breaks = byte == b'\n' || (byte == b'\r' && bytes.get(index + 1) != Some(&b'\n'));
        if breaks {
            starts.push(index as u32 + 1);
        }
    }
    starts
}

/// The number of lines in `text`.
pub(crate) fn line_count(text: &str) -> u32 {
    line_starts(text).len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kotlinc_line_break_starts_one_line() {
        // `a\r\nb\rc\nd` is the text of the kotlinc offsets tests: three breaks, four lines.
        assert_eq!(line_starts("a\r\nb\rc\nd"), vec![0, 3, 5, 7]);
        assert_eq!(line_count("a\r\nb\rc\nd"), 4);
        assert_eq!(line_starts(""), vec![0]);
        assert_eq!(line_starts("a\r"), vec![0, 2]);
        assert_eq!(line_starts("\r\r\n\n"), vec![0, 1, 3, 4]);
    }
}
