//! Compact semantic-token snapshot retained after compiler analysis is dropped.

use serde::{Deserialize, Serialize};

use crate::compiler_analysis::HighlightOccurrence;

pub const SEMANTIC_TOKEN_TYPES: [&str; 23] = [
    "namespace",
    "class",
    "enum",
    "interface",
    "struct",
    "typeParameter",
    "type",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "event",
    "function",
    "method",
    "macro",
    "keyword",
    "modifier",
    "comment",
    "string",
    "number",
    "regexp",
    "operator",
    "decorator",
];

pub const SEMANTIC_TOKEN_MODIFIERS: [&str; 10] = [
    "declaration",
    "definition",
    "readonly",
    "static",
    "deprecated",
    "abstract",
    "async",
    "modification",
    "documentation",
    "defaultLibrary",
];

/// Entries retained for one file.
///
/// Each entry is 16 bytes and stays in the open-document cache after the compiler AST is dropped.
/// A dense file can otherwise allocate millions of entries and a worker frame large enough for the
/// process to be killed. The prefix is kept so the start of the file stays highlighted.
const MAX_SEMANTIC_TOKEN_ENTRIES: usize = 64 * 1024;

/// `(line, UTF-16 start, UTF-16 length, token-type | modifiers << 8)`.
///
/// An array keeps the in-memory entry at 16 bytes and also serializes to compact JSON arrays on the
/// worker wire instead of repeating five object-field names per source token.
pub(crate) type SemanticTokenEntry = [u32; 4];

#[derive(Clone, Copy)]
pub struct SemanticTokenRange {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// Compact, already-positioned semantic-highlighting snapshot.
///
/// Positions are converted to UTF-16 once in the compiler worker. Full and range requests then
/// encode directly from this array without retaining the AST or rescanning source text.
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct SemanticTokenIndex {
    entries: Vec<SemanticTokenEntry>,
}

impl SemanticTokenIndex {
    pub fn from_file_analysis(
        source: &str,
        analysis: &crate::compiler_analysis::FileAnalysis,
        symbols: &crate::compiler_analysis::FrontendSymbols,
    ) -> Self {
        let highlight_symbols = crate::compiler_analysis::HighlightSymbols::from_source_set(
            std::slice::from_ref(analysis),
            symbols,
        );
        Self::from_source_set_file_analysis(source, analysis, symbols, &highlight_symbols)
    }

    pub fn from_source_set_file_analysis(
        source: &str,
        analysis: &crate::compiler_analysis::FileAnalysis,
        symbols: &crate::compiler_analysis::FrontendSymbols,
        highlight_symbols: &crate::compiler_analysis::HighlightSymbols,
    ) -> Self {
        Self::from_occurrences(
            source,
            analysis.highlight_occurrences(source, symbols, highlight_symbols),
        )
    }

    pub(crate) fn from_occurrences(source: &str, occurrences: Vec<HighlightOccurrence>) -> Self {
        Self {
            entries: position_semantic_tokens(source, occurrences),
        }
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn encode(&self, range: Option<SemanticTokenRange>) -> Vec<u32> {
        let entries = if let Some(range) = range {
            let start = (range.start_line, range.start_character);
            let end = (range.end_line, range.end_character);
            let first = self
                .entries
                .partition_point(|entry| (entry[0], entry[1].saturating_add(entry[2])) <= start);
            let count = self.entries[first..].partition_point(|entry| (entry[0], entry[1]) < end);
            &self.entries[first..first + count]
        } else {
            &self.entries
        };
        let mut encoded = Vec::with_capacity(entries.len().saturating_mul(5));
        let mut previous_line = 0;
        let mut previous_start = 0;
        for entry in entries {
            let line = entry[0];
            let start = entry[1];
            let delta_line = line - previous_line;
            let delta_start = if delta_line == 0 {
                start - previous_start
            } else {
                start
            };
            let packed = entry[3];
            encoded.extend_from_slice(&[
                delta_line,
                delta_start,
                entry[2],
                packed & u8::MAX as u32,
                packed >> 8,
            ]);
            previous_line = line;
            previous_start = start;
        }
        encoded
    }
}

fn position_semantic_tokens(
    source: &str,
    tokens: Vec<HighlightOccurrence>,
) -> Vec<SemanticTokenEntry> {
    let mut entries = Vec::with_capacity(tokens.len().min(MAX_SEMANTIC_TOKEN_ENTRIES));
    let mut byte = 0usize;
    let mut line = 0u32;
    let mut character = 0u32;
    let mut previous_was_cr = false;
    for token in tokens {
        if entries.len() == MAX_SEMANTIC_TOKEN_ENTRIES {
            break;
        }
        advance_position(
            &source[byte..token.span.lo as usize],
            &mut line,
            &mut character,
            &mut previous_was_cr,
        );
        let start_line = line;
        let start = character;
        advance_position(
            &source[token.span.lo as usize..token.span.hi as usize],
            &mut line,
            &mut character,
            &mut previous_was_cr,
        );
        if line == start_line {
            entries.push([
                start_line,
                start,
                character - start,
                token.kind as u32 | u32::from(token.modifiers.bits()) << 8,
            ]);
        }
        byte = token.span.hi as usize;
    }
    entries
}

pub(crate) fn advance_position(
    text: &str,
    line: &mut u32,
    character: &mut u32,
    previous_was_cr: &mut bool,
) {
    for ch in text.chars() {
        match ch {
            '\r' => {
                *line = line.saturating_add(1);
                *character = 0;
                *previous_was_cr = true;
            }
            '\n' => {
                if !*previous_was_cr {
                    *line = line.saturating_add(1);
                }
                *character = 0;
                *previous_was_cr = false;
            }
            _ => {
                *character = character.saturating_add(ch.len_utf16() as u32);
                *previous_was_cr = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler_analysis::{HighlightKind, HighlightModifiers};
    use krusty::diag::Span;

    #[test]
    fn semantic_token_index_keeps_the_prefix_at_the_entry_cap() {
        let count = MAX_SEMANTIC_TOKEN_ENTRIES + 8;
        let mut source = String::with_capacity(count * 2);
        let mut occurrences = Vec::with_capacity(count);
        for index in 0..count {
            let lo = (index * 2) as u32;
            occurrences.push(HighlightOccurrence {
                span: Span::new(lo, lo + 1),
                kind: HighlightKind::Variable,
                modifiers: HighlightModifiers::default(),
            });
            source.push_str("a ");
        }

        let index = SemanticTokenIndex::from_occurrences(&source, occurrences);

        assert_eq!(index.entry_count(), MAX_SEMANTIC_TOKEN_ENTRIES);
        let encoded = index.encode(None);
        assert_eq!(encoded.len(), MAX_SEMANTIC_TOKEN_ENTRIES * 5);
        assert_eq!(&encoded[..5], &[0, 0, 1, HighlightKind::Variable as u32, 0]);
        assert_eq!(
            &encoded[encoded.len() - 5..],
            &[0, 2, 1, HighlightKind::Variable as u32, 0]
        );
    }
}
