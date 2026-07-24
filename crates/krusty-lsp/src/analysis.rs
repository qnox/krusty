//! Compact semantic data retained by interactive language-server queries.

use std::collections::{HashMap, HashSet};

use crate::compiler_analysis::{
    analyze_standalone_source_set, CompletionSymbols, DefinitionOccurrence, DefinitionSymbols,
    DefinitionTarget, FileAnalysis, FrontendSymbols, HighlightOccurrence, HighlightSymbols,
};
use krusty::diag::{Diagnostic, Span};
use krusty::types::Ty;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Deserialize, Serialize)]
struct HoverEntry {
    lo: u32,
    hi: u32,
    type_index: u32,
}

/// Compact semantic snapshot retained for hover queries after full compiler analysis is dropped.
#[derive(Default, Deserialize, Serialize)]
pub struct HoverIndex {
    entries: Vec<HoverEntry>,
    type_names: Vec<String>,
}

pub struct Hover<'a> {
    pub span: Span,
    pub type_name: &'a str,
}

const NO_COMPLETION_TYPE: u32 = 0x003f_ffff;
const MEMBER_COMPLETION_SLOT: u32 = 1 << 31;
const MAX_SOURCE_SET_COMPLETION_ENTRIES: usize = 32 * 1024;
const MAX_SOURCE_SET_COMPLETION_WIRE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SOURCE_SET_DEFINITION_ENTRIES: usize = 256 * 1024;

#[derive(Default)]
pub(crate) struct CompletionBudget {
    entries: usize,
    wire_bytes: usize,
}

#[derive(Default)]
pub(crate) struct DefinitionBudget {
    entries: usize,
}

impl DefinitionBudget {
    fn remaining(&self) -> usize {
        MAX_SOURCE_SET_DEFINITION_ENTRIES.saturating_sub(self.entries)
    }
}

impl CompletionBudget {
    fn reserve(&mut self, label: &str, detail: &str, result_type: Option<&str>) -> bool {
        let string_bytes = label
            .len()
            .saturating_add(detail.len())
            .saturating_add(result_type.map_or(0, str::len));
        let wire_bytes = 96usize.saturating_add(string_bytes.saturating_mul(6));
        if self.entries >= MAX_SOURCE_SET_COMPLETION_ENTRIES
            || wire_bytes > MAX_SOURCE_SET_COMPLETION_WIRE_BYTES.saturating_sub(self.wire_bytes)
        {
            return false;
        }
        self.entries += 1;
        self.wire_bytes += wire_bytes;
        true
    }
}

/// `(scope lo, scope hi, declared at, label id, detail id, kind | result-type id << 8)`.
type CompletionEntry = [u32; 6];
/// `(receiver-type id, label id, detail id, kind)`.
type CompletionMemberEntry = [u32; 4];

/// Compact completion catalog retained after compiler analysis is dropped.
#[derive(Default, Deserialize, Serialize)]
pub struct CompletionIndex {
    entries: Vec<CompletionEntry>,
    members: Vec<CompletionMemberEntry>,
    strings: Vec<String>,
    incomplete: bool,
}

pub struct Completion<'a> {
    pub slot: u32,
    pub label: &'a str,
    pub kind: u8,
}

/// `(source lo, source hi, target file, target lo, target hi)`.
type DefinitionEntry = [u32; 5];

#[derive(Default, Deserialize, Serialize)]
pub struct DefinitionIndex {
    entries: Vec<DefinitionEntry>,
}

pub struct DefinitionTargets<'a> {
    entries: std::slice::Iter<'a, DefinitionEntry>,
}

impl Iterator for DefinitionTargets<'_> {
    type Item = DefinitionTarget;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.entries.next()?;
        Some(DefinitionTarget {
            file: entry[2],
            span: Span::new(entry[3], entry[4]),
        })
    }
}

impl DefinitionIndex {
    fn from_occurrences(
        occurrences: Vec<DefinitionOccurrence>,
        budget: &mut DefinitionBudget,
    ) -> Self {
        let available = MAX_SOURCE_SET_DEFINITION_ENTRIES.saturating_sub(budget.entries);
        let mut entries = occurrences
            .into_iter()
            .map(|occurrence| {
                [
                    occurrence.span.lo,
                    occurrence.span.hi,
                    occurrence.target.file,
                    occurrence.target.span.lo,
                    occurrence.target.span.hi,
                ]
            })
            .collect::<Vec<_>>();
        entries.sort_unstable();
        entries.dedup();
        entries.truncate(available);
        budget.entries += entries.len();
        Self { entries }
    }

    pub fn get(&self, byte_offset: u32) -> DefinitionTargets<'_> {
        let upper = self
            .entries
            .partition_point(|entry| entry[0] <= byte_offset);
        let Some(candidate) = upper
            .checked_sub(1)
            .and_then(|index| self.entries.get(index))
        else {
            return DefinitionTargets {
                entries: self.entries[0..0].iter(),
            };
        };
        if byte_offset >= candidate[1] {
            return DefinitionTargets {
                entries: self.entries[0..0].iter(),
            };
        }
        let source = (candidate[0], candidate[1]);
        let start = self
            .entries
            .partition_point(|entry| (entry[0], entry[1]) < source);
        let end = self
            .entries
            .partition_point(|entry| (entry[0], entry[1]) <= source);
        DefinitionTargets {
            entries: self.entries[start..end].iter(),
        }
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

impl CompletionIndex {
    #[cfg(test)]
    pub(crate) fn from_file_analysis(
        source: &str,
        analysis: &FileAnalysis,
        symbols: &CompletionSymbols,
    ) -> Self {
        Self::from_file_analysis_with_budget(
            source,
            analysis,
            symbols,
            &mut CompletionBudget::default(),
        )
    }

    pub(crate) fn from_file_analysis_with_budget(
        source: &str,
        analysis: &FileAnalysis,
        symbols: &CompletionSymbols,
        budget: &mut CompletionBudget,
    ) -> Self {
        let scoped = analysis.scoped_completion_symbols(source, symbols);
        let file_span = Span::new(0, source.len() as u32);
        let receiver_names = completion_receiver_names(source);
        let member_owners: HashSet<_> = scoped
            .iter()
            .filter(|symbol| {
                symbol.scope != file_span || receiver_names.contains(symbol.label.as_str())
            })
            .filter_map(|symbol| symbol.result_type.clone())
            .collect();
        let mut strings = Vec::new();
        let mut string_ids = HashMap::new();
        let mut intern = |value: &str| {
            if let Some(&id) = string_ids.get(value) {
                id
            } else {
                let id = strings.len() as u32;
                strings.push(value.to_string());
                string_ids.insert(value.to_string(), id);
                id
            }
        };
        let mut incomplete = false;
        let entries = scoped
            .into_iter()
            .filter_map(|symbol| {
                if !budget.reserve(&symbol.label, &symbol.detail, symbol.result_type.as_deref()) {
                    incomplete = true;
                    return None;
                }
                let label = intern(&symbol.label);
                let detail = intern(&symbol.detail);
                let result_type = symbol
                    .result_type
                    .as_deref()
                    .map(&mut intern)
                    .unwrap_or(NO_COMPLETION_TYPE);
                Some([
                    symbol.scope.lo,
                    symbol.scope.hi,
                    symbol.declared_at,
                    label,
                    detail,
                    symbol.kind as u32 | result_type << 8 | u32::from(symbol.priority) << 30,
                ])
            })
            .collect();
        let members = symbols
            .members()
            .filter(|(owner, _, _, _)| member_owners.contains(*owner))
            .filter_map(|(owner, label, detail, kind)| {
                if !budget.reserve(label, detail, Some(owner)) {
                    incomplete = true;
                    return None;
                }
                Some([intern(owner), intern(label), intern(detail), kind as u32])
            })
            .collect();
        Self {
            entries,
            members,
            strings,
            incomplete,
        }
    }

    pub fn complete(&self, source: &str, offset: u32) -> Vec<Completion<'_>> {
        let Some(context) = completion_context(source, offset as usize) else {
            return Vec::new();
        };
        if let Some(receiver) = context.receiver {
            let Some(receiver_type) = self
                .entries
                .iter()
                .filter(|entry| {
                    self.strings[entry[3] as usize] == receiver
                        && entry[0] <= offset
                        && offset <= entry[1]
                        && entry[2] <= offset
                })
                .min_by_key(|entry| {
                    (
                        entry[1].saturating_sub(entry[0]),
                        std::cmp::Reverse(entry[5] >> 30),
                    )
                })
                .map(|entry| (entry[5] >> 8) & NO_COMPLETION_TYPE)
                .filter(|&type_id| type_id != NO_COMPLETION_TYPE)
            else {
                return Vec::new();
            };
            let mut result: Vec<_> = self
                .members
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    entry[0] == receiver_type
                        && self.strings[entry[1] as usize].starts_with(context.prefix)
                })
                .map(|(index, entry)| Completion {
                    slot: MEMBER_COMPLETION_SLOT | index as u32,
                    label: &self.strings[entry[1] as usize],
                    kind: entry[3] as u8,
                })
                .collect();
            result.sort_unstable_by_key(|candidate| candidate.label);
            result.dedup_by_key(|candidate| candidate.label);
            return result;
        }

        let mut best_by_label = HashMap::<&str, (usize, u32, u32)>::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let label = self.strings[entry[3] as usize].as_str();
            if entry[0] > offset
                || offset > entry[1]
                || entry[2] > offset
                || !label.starts_with(context.prefix)
            {
                continue;
            }
            let width = entry[1].saturating_sub(entry[0]);
            let priority = entry[5] >> 30;
            match best_by_label.get(label) {
                Some((_, best_width, best_priority))
                    if *best_width < width
                        || (*best_width == width && *best_priority >= priority) => {}
                _ => {
                    best_by_label.insert(label, (index, width, priority));
                }
            }
        }
        let mut result: Vec<_> = best_by_label
            .into_iter()
            .map(|(label, (index, _, _))| Completion {
                slot: index as u32,
                label,
                kind: self.entries[index][5] as u8,
            })
            .collect();
        result.sort_unstable_by_key(|candidate| candidate.label);
        result
    }

    pub fn resolve(&self, slot: u32, expected_label: &str) -> Option<&str> {
        let (label, detail) = if slot & MEMBER_COMPLETION_SLOT != 0 {
            let entry = self
                .members
                .get((slot & !MEMBER_COMPLETION_SLOT) as usize)?;
            (entry[1], entry[2])
        } else {
            let entry = self.entries.get(slot as usize)?;
            (entry[3], entry[4])
        };
        (self.strings.get(label as usize)?.as_str() == expected_label)
            .then(|| self.strings.get(detail as usize).map(String::as_str))
            .flatten()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn is_incomplete(&self) -> bool {
        self.incomplete
    }
}

struct CompletionContext<'a> {
    receiver: Option<&'a str>,
    prefix: &'a str,
}

fn completion_context(source: &str, offset: usize) -> Option<CompletionContext<'_>> {
    if offset > source.len() || !source.is_char_boundary(offset) {
        return None;
    }
    let prefix_start = identifier_start(source, offset);
    let prefix = &source[prefix_start..offset];
    let before_prefix = &source[..prefix_start];
    let before_dot = before_prefix
        .strip_suffix("?.")
        .or_else(|| before_prefix.strip_suffix('.'));
    let receiver = before_dot.and_then(|before_receiver| {
        let receiver_end = before_receiver.len();
        let receiver_start = identifier_start(before_receiver, receiver_end);
        (receiver_start != receiver_end).then_some(&before_receiver[receiver_start..receiver_end])
    });
    Some(CompletionContext { receiver, prefix })
}

fn identifier_start(source: &str, end: usize) -> usize {
    source[..end]
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            (!character.is_alphanumeric() && character != '_')
                .then_some(index + character.len_utf8())
        })
        .unwrap_or(0)
}

fn completion_receiver_names(source: &str) -> HashSet<&str> {
    source
        .match_indices('.')
        .filter_map(|(dot, _)| {
            let end = dot.saturating_sub(usize::from(source[..dot].ends_with('?')));
            let start = identifier_start(source, end);
            (start != end).then_some(&source[start..end])
        })
        .collect()
}

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

/// `(line, UTF-16 start, UTF-16 length, token-type | modifiers << 8)`.
///
/// An array keeps the in-memory entry at 16 bytes and also serializes to compact JSON arrays on the
/// worker wire instead of repeating five object-field names per source token.
type SemanticTokenEntry = [u32; 4];

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
#[derive(Default, Deserialize, Serialize)]
pub struct SemanticTokenIndex {
    entries: Vec<SemanticTokenEntry>,
}

impl SemanticTokenIndex {
    pub fn from_file_analysis(
        source: &str,
        analysis: &FileAnalysis,
        symbols: &FrontendSymbols,
    ) -> Self {
        let highlight_symbols =
            HighlightSymbols::from_source_set(&[source], std::slice::from_ref(analysis), symbols);
        Self::from_source_set_file_analysis(source, analysis, symbols, &highlight_symbols)
    }

    pub fn from_source_set_file_analysis(
        source: &str,
        analysis: &FileAnalysis,
        symbols: &FrontendSymbols,
        highlight_symbols: &HighlightSymbols,
    ) -> Self {
        Self::from_occurrences(
            source,
            analysis.highlight_occurrences(source, symbols, highlight_symbols),
        )
    }

    fn from_occurrences(source: &str, occurrences: Vec<HighlightOccurrence>) -> Self {
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
    let mut entries = Vec::with_capacity(tokens.len());
    let mut byte = 0usize;
    let mut line = 0u32;
    let mut character = 0u32;
    let mut previous_was_cr = false;
    for token in tokens {
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

fn advance_position(text: &str, line: &mut u32, character: &mut u32, previous_was_cr: &mut bool) {
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

impl HoverIndex {
    pub fn from_file_analysis(analysis: &FileAnalysis) -> Self {
        Self::from_typed_expressions(analysis.typed_expressions(), analysis.file.expr_spans.len())
    }

    fn from_typed_expressions(
        typed_expressions: impl Iterator<Item = (Span, Ty)>,
        capacity: usize,
    ) -> Self {
        let mut unique_types = Vec::new();
        let mut type_indices = HashMap::new();
        let mut entries = Vec::with_capacity(capacity);
        for (span, ty) in typed_expressions {
            if ty == Ty::Error {
                continue;
            }
            let type_index = match type_indices.get(&ty) {
                Some(&index) => index,
                None => {
                    let index = unique_types.len() as u32;
                    unique_types.push(ty);
                    type_indices.insert(ty, index);
                    index
                }
            };
            entries.push(HoverEntry {
                lo: span.lo,
                hi: span.hi,
                type_index,
            });
        }
        Self {
            entries,
            type_names: unique_types.into_iter().map(source_type_name).collect(),
        }
    }

    pub fn get(&self, byte_offset: u32) -> Option<Hover<'_>> {
        self.entries
            .iter()
            .filter(|entry| {
                entry.lo <= byte_offset
                    && (byte_offset < entry.hi || (entry.lo == entry.hi && byte_offset == entry.lo))
            })
            .min_by_key(|entry| entry.hi.saturating_sub(entry.lo))
            .map(|entry| Hover {
                span: Span::new(entry.lo, entry.hi),
                type_name: &self.type_names[entry.type_index as usize],
            })
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn type_count(&self) -> usize {
        self.type_names.len()
    }
}

pub struct DocumentAnalysis {
    pub diagnostics: Vec<Diagnostic>,
    pub hover: HoverIndex,
    pub completion: CompletionIndex,
    pub semantic_tokens: SemanticTokenIndex,
    pub definitions: DefinitionIndex,
}

pub(crate) struct SourceSetIndexes<'a> {
    symbols: &'a FrontendSymbols,
    highlights: &'a HighlightSymbols,
    definitions: &'a DefinitionSymbols,
    completions: &'a CompletionSymbols,
}

impl<'a> SourceSetIndexes<'a> {
    pub(crate) fn new(
        symbols: &'a FrontendSymbols,
        highlights: &'a HighlightSymbols,
        definitions: &'a DefinitionSymbols,
        completions: &'a CompletionSymbols,
    ) -> Self {
        Self {
            symbols,
            highlights,
            definitions,
            completions,
        }
    }
}

pub(crate) struct AnalysisBudgets {
    completion: CompletionBudget,
    definition: DefinitionBudget,
}

impl AnalysisBudgets {
    pub(crate) fn new() -> Self {
        Self {
            completion: CompletionBudget::default(),
            definition: DefinitionBudget::default(),
        }
    }
}

impl DocumentAnalysis {
    pub(crate) fn from_file_analysis(
        source: &str,
        analysis: FileAnalysis,
        file_index: u32,
        indexes: &SourceSetIndexes<'_>,
        budgets: &mut AnalysisBudgets,
    ) -> Self {
        let hover = HoverIndex::from_file_analysis(&analysis);
        let completion = CompletionIndex::from_file_analysis_with_budget(
            source,
            &analysis,
            indexes.completions,
            &mut budgets.completion,
        );
        let semantic = analysis.semantic_occurrences(
            source,
            file_index,
            indexes.symbols,
            indexes.highlights,
            indexes.definitions,
            budgets.definition.remaining(),
        );
        let semantic_tokens = SemanticTokenIndex::from_occurrences(source, semantic.highlights);
        let definitions =
            DefinitionIndex::from_occurrences(semantic.definitions, &mut budgets.definition);
        Self {
            diagnostics: analysis.diagnostics,
            hover,
            completion,
            semantic_tokens,
            definitions,
        }
    }

    pub fn with_diagnostics(diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            diagnostics,
            hover: HoverIndex::default(),
            completion: CompletionIndex::default(),
            semantic_tokens: SemanticTokenIndex::default(),
            definitions: DefinitionIndex::default(),
        }
    }

    pub fn empty() -> Self {
        Self::with_diagnostics(Vec::new())
    }
}

/// Analyze one source in an open source set and retain only data needed by editor queries.
pub fn analyze_for_lsp(sources: &[&str]) -> Vec<DocumentAnalysis> {
    let analysis = analyze_standalone_source_set(sources);
    let highlight_symbols =
        HighlightSymbols::from_source_set(sources, &analysis.files, &analysis.symbols);
    let definition_symbols =
        DefinitionSymbols::from_source_set(sources, &analysis.files, &analysis.symbols);
    let completion_symbols = CompletionSymbols::from_source_set(sources, &analysis.files);
    let indexes = SourceSetIndexes::new(
        &analysis.symbols,
        &highlight_symbols,
        &definition_symbols,
        &completion_symbols,
    );
    let mut budgets = AnalysisBudgets::new();
    analysis
        .files
        .into_iter()
        .zip(sources)
        .enumerate()
        .map(|(file_index, (file, source))| {
            DocumentAnalysis::from_file_analysis(
                source,
                file,
                file_index as u32,
                &indexes,
                &mut budgets,
            )
        })
        .collect()
}

fn source_type_name(ty: Ty) -> String {
    match ty {
        Ty::Int => "Int".to_string(),
        Ty::Byte => "Byte".to_string(),
        Ty::Short => "Short".to_string(),
        Ty::Long => "Long".to_string(),
        Ty::Float => "Float".to_string(),
        Ty::Double => "Double".to_string(),
        Ty::Boolean => "Boolean".to_string(),
        Ty::Char => "Char".to_string(),
        Ty::UInt => "UInt".to_string(),
        Ty::ULong => "ULong".to_string(),
        Ty::String => "String".to_string(),
        Ty::Unit => "Unit".to_string(),
        Ty::Obj(name, args) => {
            let mut rendered = name.render().replace('/', ".");
            if !args.is_empty() {
                rendered.push('<');
                for (index, arg) in args.iter().enumerate() {
                    if index != 0 {
                        rendered.push_str(", ");
                    }
                    rendered.push_str(&source_type_name(*arg));
                }
                rendered.push('>');
            }
            rendered
        }
        Ty::Null => "Nothing?".to_string(),
        Ty::Nothing => "Nothing".to_string(),
        Ty::Error => "<error>".to_string(),
        Ty::Fun(signature) => {
            let mut rendered = if signature.suspend {
                "suspend (".to_string()
            } else {
                "(".to_string()
            };
            for (index, parameter) in signature.params.iter().enumerate() {
                if index != 0 {
                    rendered.push_str(", ");
                }
                rendered.push_str(&source_type_name(*parameter));
            }
            rendered.push_str(") -> ");
            rendered.push_str(&source_type_name(signature.ret));
            rendered
        }
        Ty::Nullable(inner) => format!("{}?", source_type_name(*inner)),
        Ty::TyParam(name, _) => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler_analysis::CompletionKind;

    fn decoded_tokens(index: &SemanticTokenIndex) -> Vec<(u32, u32, u32, u32, u32)> {
        let mut line = 0;
        let mut start = 0;
        index
            .encode(None)
            .chunks_exact(5)
            .map(|token| {
                line += token[0];
                start = if token[0] == 0 {
                    start + token[1]
                } else {
                    token[1]
                };
                (line, start, token[2], token[3], token[4])
            })
            .collect()
    }

    #[test]
    fn definition_snapshot_uses_compact_file_and_span_entries() {
        assert_eq!(std::mem::size_of::<DefinitionEntry>(), 20);
        let source = "data class User(val name: String)\n\
                      fun greet(user: User): String = user.name\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        for (query, target_lo, target_hi) in [
            (source.rfind("User").unwrap() as u32, 11, 15),
            (source.rfind("user").unwrap() as u32, 44, 48),
            (source.rfind("name").unwrap() as u32, 20, 24),
        ] {
            assert_eq!(
                analysis.definitions.get(query).collect::<Vec<_>>(),
                vec![DefinitionTarget {
                    file: 0,
                    span: Span::new(target_lo, target_hi),
                }]
            );
        }
    }

    #[test]
    fn definition_snapshot_respects_the_source_set_entry_budget() {
        let mut budget = DefinitionBudget {
            entries: MAX_SOURCE_SET_DEFINITION_ENTRIES - 1,
        };
        let occurrences = vec![
            DefinitionOccurrence {
                span: Span::new(0, 1),
                target: DefinitionTarget {
                    file: 0,
                    span: Span::new(4, 5),
                },
            },
            DefinitionOccurrence {
                span: Span::new(2, 3),
                target: DefinitionTarget {
                    file: 0,
                    span: Span::new(6, 7),
                },
            },
        ];
        let index = DefinitionIndex::from_occurrences(occurrences, &mut budget);
        assert_eq!(index.entry_count(), 1);
        assert_eq!(budget.entries, MAX_SOURCE_SET_DEFINITION_ENTRIES);
    }

    #[test]
    fn hover_index_returns_smallest_typed_expression() {
        let source = "fun box(): Int { val answer = 40 + 2; return answer }";
        let mut analysis = analyze_standalone_source_set(&[source]).files;
        let analysis = analysis.remove(0);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );

        let index = HoverIndex::from_file_analysis(&analysis);
        let offset = source.rfind("answer").unwrap() as u32 + 1;
        let hover = index.get(offset).expect("hover over local read");
        assert_eq!(hover.type_name, "Int");
        assert_eq!(
            &source[hover.span.lo as usize..hover.span.hi as usize],
            "answer"
        );
    }

    #[test]
    fn hover_index_deduplicates_type_names_into_twelve_byte_entries() {
        let analysis = analyze_standalone_source_set(&["fun box(): Int = (40 + 2) * 1"])
            .files
            .remove(0);
        let index = HoverIndex::from_file_analysis(&analysis);
        assert!(index.entry_count() >= 5);
        assert_eq!(index.type_count(), 1);
        assert_eq!(std::mem::size_of::<HoverEntry>(), 12);
    }

    #[test]
    fn hover_formats_null_as_nullable_nothing() {
        let source = "fun box(): String? = null";
        let analysis = analyze_standalone_source_set(&[source]).files.remove(0);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let index = HoverIndex::from_file_analysis(&analysis);
        let hover = index
            .get(source.rfind("null").unwrap() as u32 + 1)
            .expect("hover over null expression");
        assert_eq!(hover.type_name, "Nothing?");
    }

    #[test]
    fn completion_survives_an_incomplete_safe_member_access() {
        let source = concat!(
            "class User(val name: String) { fun greeting(): String = name }\n",
            "fun demo(user: User) = user?."
        );
        let analysis = analyze_standalone_source_set(&[source]);
        assert!(
            analysis.files[0].types.is_none(),
            "the test must exercise the parser-recovery snapshot"
        );
        let symbols = CompletionSymbols::from_source_set(&[source], &analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let candidates = index.complete(source, source.len() as u32);

        assert!(candidates.iter().any(|candidate| candidate.label == "name"
            && candidate.kind == CompletionKind::Property as u8));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.label == "greeting"
                && candidate.kind == CompletionKind::Method as u8));
    }

    #[test]
    fn completion_snapshot_interns_strings_into_compact_array_entries() {
        let source = "fun demo(user: String) { val local: String = user; loc }";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&[source], &analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let offset = source.rfind("loc").unwrap() as u32 + 3;
        let candidates = index.complete(source, offset);

        assert!(candidates.iter().any(|candidate| candidate.label == "local"
            && candidate.kind == CompletionKind::Variable as u8));
        assert_eq!(std::mem::size_of::<CompletionEntry>(), 24);
        assert_eq!(std::mem::size_of::<CompletionMemberEntry>(), 16);
        let json = serde_json::to_value(&index).unwrap();
        assert_eq!(json["entries"][0].as_array().unwrap().len(), 6);
        assert!(
            json["strings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|value| *value == "String")
                .count()
                <= 1
        );
    }

    #[test]
    fn completion_includes_inherited_members() {
        let source = concat!(
            "open class Base(val inherited: Int)\n",
            "class Child : Base(1)\n",
            "fun demo(child: Child) = child."
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&[source], &analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let candidates = index.complete(source, source.len() as u32);

        assert!(candidates
            .iter()
            .any(|candidate| candidate.label == "inherited"
                && candidate.kind == CompletionKind::Property as u8));
    }

    #[test]
    fn completion_does_not_offer_unimported_cross_package_symbols() {
        let sources = [
            "package hidden\nfun secret(): Int = 1",
            "package visible\nfun use(): Int = sec",
            "package consumer\nimport hidden.secret\nfun use(): Int = sec",
        ];
        let analysis = analyze_standalone_source_set(&sources);
        let symbols = CompletionSymbols::from_source_set(&sources, &analysis.files);
        let index = CompletionIndex::from_file_analysis(sources[1], &analysis.files[1], &symbols);
        let candidates = index.complete(sources[1], sources[1].len() as u32);

        assert!(candidates
            .iter()
            .all(|candidate| candidate.label != "secret"));

        let imported =
            CompletionIndex::from_file_analysis(sources[2], &analysis.files[2], &symbols);
        assert!(imported
            .complete(sources[2], sources[2].len() as u32)
            .iter()
            .any(|candidate| candidate.label == "secret"));
    }

    #[test]
    fn completion_matches_the_official_constant_item_kind() {
        let source = "const val FLAG: Int = 1\nfun use(): Int = FL";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&[source], &analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);

        assert!(index
            .complete(source, source.len() as u32)
            .iter()
            .any(|candidate| candidate.label == "FLAG"
                && candidate.kind == CompletionKind::Constant as u8));
    }

    #[test]
    fn completion_keeps_class_and_companion_lexical_contexts_distinct() {
        let class_source = "class Box<T> { fun value() = T }";
        let class_analysis = analyze_standalone_source_set(&[class_source]);
        let class_symbols =
            CompletionSymbols::from_source_set(&[class_source], &class_analysis.files);
        let class_index = CompletionIndex::from_file_analysis(
            class_source,
            &class_analysis.files[0],
            &class_symbols,
        );
        let type_parameter_offset = class_source.rfind('T').unwrap() as u32 + 1;
        assert!(class_index
            .complete(class_source, type_parameter_offset)
            .iter()
            .any(|candidate| candidate.label == "T"
                && candidate.kind == CompletionKind::TypeParameter as u8));

        let companion_source = concat!(
            "class Owner { val instance: Int = 1; companion object { ",
            "val shared: Int = 2; fun use(): Int = sh } }"
        );
        let companion_analysis = analyze_standalone_source_set(&[companion_source]);
        let companion_symbols =
            CompletionSymbols::from_source_set(&[companion_source], &companion_analysis.files);
        let companion_index = CompletionIndex::from_file_analysis(
            companion_source,
            &companion_analysis.files[0],
            &companion_symbols,
        );
        let offset = companion_source.rfind("sh").unwrap() as u32 + 2;
        let candidates = companion_index.complete(companion_source, offset);
        assert!(candidates
            .iter()
            .any(|candidate| candidate.label == "shared"));
        assert!(candidates
            .iter()
            .all(|candidate| candidate.label != "instance"));
    }

    #[test]
    fn completion_retains_only_member_catalogs_referenced_by_the_document() {
        let sources = [
            "class Alpha(val alphaMember: Int)",
            "class Beta(val betaMember: Int)",
            "fun use(alpha: Alpha) = alpha.",
        ];
        let analysis = analyze_standalone_source_set(&sources);
        let symbols = CompletionSymbols::from_source_set(&sources, &analysis.files);
        let index = CompletionIndex::from_file_analysis(sources[2], &analysis.files[2], &symbols);
        let json = serde_json::to_value(&index).unwrap();
        let member_labels: Vec<_> = json["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                let label = entry[1].as_u64().unwrap() as usize;
                json["strings"][label].as_str().unwrap()
            })
            .collect();

        assert!(member_labels.contains(&"alphaMember"));
        assert!(!member_labels.contains(&"betaMember"));
    }

    #[test]
    fn completion_omits_inaccessible_private_declarations() {
        let sources = [
            concat!(
                "package hidden\n",
                "private fun hidden(): Int = 1\n",
                "class Secret(private val value: Int)\n",
                "fun String.secretExtension(): Int = 1",
            ),
            "package visible\nfun use(secret: hidden.Secret, text: String) = secret.",
            "package visible\nfun use(text: String) = text.",
        ];
        let analysis = analyze_standalone_source_set(&sources);
        let symbols = CompletionSymbols::from_source_set(&sources, &analysis.files);
        let index = CompletionIndex::from_file_analysis(sources[1], &analysis.files[1], &symbols);
        let candidates = index.complete(sources[1], sources[1].len() as u32);

        assert!(candidates
            .iter()
            .all(|candidate| candidate.label != "value"));
        assert!(serde_json::to_value(&index).unwrap()["strings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|value| value != "hidden"));

        let extension_index =
            CompletionIndex::from_file_analysis(sources[2], &analysis.files[2], &symbols);
        assert!(extension_index
            .complete(sources[2], sources[2].len() as u32)
            .iter()
            .all(|candidate| candidate.label != "secretExtension"));
    }

    #[test]
    fn completion_keeps_same_named_classes_in_separate_packages() {
        let sources = [
            "package p\nclass Same(val pOnly: Int)",
            "package q\nclass Same(val qOnly: Int)",
            "package use\nimport q.Same\nfun use(value: Same) = value.",
            "package wildcard\nimport q.*\nfun use(value: Same) = value.",
        ];
        let analysis = analyze_standalone_source_set(&sources);
        let symbols = CompletionSymbols::from_source_set(&sources, &analysis.files);
        let index = CompletionIndex::from_file_analysis(sources[2], &analysis.files[2], &symbols);
        let candidates = index.complete(sources[2], sources[2].len() as u32);

        assert!(candidates
            .iter()
            .any(|candidate| candidate.label == "qOnly"));
        assert!(candidates
            .iter()
            .all(|candidate| candidate.label != "pOnly"));

        let wildcard =
            CompletionIndex::from_file_analysis(sources[3], &analysis.files[3], &symbols);
        let wildcard_candidates = wildcard.complete(sources[3], sources[3].len() as u32);
        assert!(wildcard_candidates
            .iter()
            .any(|candidate| candidate.label == "qOnly"));
        assert!(wildcard_candidates
            .iter()
            .all(|candidate| candidate.label != "pOnly"));
    }

    #[test]
    fn completion_prefers_a_root_block_local_over_a_class_member() {
        let source = "class C(val x: Int) { fun use() { val x: String = \"\"; x } }";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&[source], &analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let offset = source.rfind('x').unwrap() as u32 + 1;
        let candidate = index
            .complete(source, offset)
            .into_iter()
            .find(|candidate| candidate.label == "x")
            .unwrap();

        assert_eq!(index.resolve(candidate.slot, "x"), Some("val x: String"));
    }

    #[test]
    fn completion_uses_qualified_property_result_owners() {
        let source = concat!(
            "package p\n",
            "class Other(val found: Int)\n",
            "class Holder(val other: Other) { fun use() = other. }\n",
            "val top: Other = Other(1)\n",
            "fun topUse() = top."
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&[source], &analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        for marker in ["other.", "top."] {
            let offset = source.find(marker).unwrap() as u32 + marker.len() as u32;
            assert!(index
                .complete(source, offset)
                .iter()
                .any(|candidate| candidate.label == "found"));
        }
    }

    #[test]
    fn receiver_completion_uses_lexical_priority_for_shadowing() {
        let source = concat!(
            "class A(val aOnly: Int)\n",
            "class B(val bOnly: Int)\n",
            "class C(val x: A) { fun use() { val x: B = B(1); x. } }"
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&[source], &analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let offset = source.rfind("x.").unwrap() as u32 + 2;
        let candidates = index.complete(source, offset);

        assert!(candidates
            .iter()
            .any(|candidate| candidate.label == "bOnly"));
        assert!(candidates
            .iter()
            .all(|candidate| candidate.label != "aOnly"));
    }

    #[test]
    fn incomplete_receiver_completion_recovers_constructor_inferred_local_type() {
        let source = concat!(
            "class User(val name: String)\n",
            "fun use() { val user = User(\"\"); user. }"
        );
        let analysis = analyze_standalone_source_set(&[source]);
        assert!(analysis.files[0].types.is_none());
        let symbols = CompletionSymbols::from_source_set(&[source], &analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let offset = source.rfind("user.").unwrap() as u32 + 5;

        assert!(index
            .complete(source, offset)
            .iter()
            .any(|candidate| candidate.label == "name"));
    }

    #[test]
    fn incomplete_constructor_recovery_declines_a_callable_shadowing_the_class() {
        let source = concat!(
            "class User(val wrong: Int)\n",
            "class Actual(val right: Int)\n",
            "fun use() { fun User(): Actual = Actual(1); val x = User(); x. }"
        );
        let analysis = analyze_standalone_source_set(&[source]);
        assert!(analysis.files[0].types.is_none());
        let symbols = CompletionSymbols::from_source_set(&[source], &analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let offset = source.rfind("x.").unwrap() as u32 + 2;

        assert!(index
            .complete(source, offset)
            .iter()
            .all(|candidate| candidate.label != "wrong"));

        let value_source = concat!(
            "class User(val wrong: Int)\n",
            "class Actual(val right: Int)\n",
            "fun use() { val User: () -> Actual = { Actual(1) }; ",
            "val x = User(); x. }"
        );
        let value_analysis = analyze_standalone_source_set(&[value_source]);
        assert!(value_analysis.files[0].types.is_none());
        let value_symbols =
            CompletionSymbols::from_source_set(&[value_source], &value_analysis.files);
        let value_index = CompletionIndex::from_file_analysis(
            value_source,
            &value_analysis.files[0],
            &value_symbols,
        );
        let value_offset = value_source.rfind("x.").unwrap() as u32 + 2;
        assert!(value_index
            .complete(value_source, value_offset)
            .iter()
            .all(|candidate| candidate.label != "wrong"));

        let parameter_source = concat!(
            "class User(val wrong: Int)\n",
            "class Actual(val right: Int)\n",
            "fun outer() { fun inner(User: () -> Actual) { ",
            "val x = User(); x. } }"
        );
        let parameter_analysis = analyze_standalone_source_set(&[parameter_source]);
        assert!(parameter_analysis.files[0].types.is_none());
        let parameter_symbols =
            CompletionSymbols::from_source_set(&[parameter_source], &parameter_analysis.files);
        let parameter_index = CompletionIndex::from_file_analysis(
            parameter_source,
            &parameter_analysis.files[0],
            &parameter_symbols,
        );
        let parameter_offset = parameter_source.rfind("x.").unwrap() as u32 + 2;
        assert!(parameter_index
            .complete(parameter_source, parameter_offset)
            .iter()
            .all(|candidate| candidate.label != "wrong"));
    }

    #[test]
    fn completion_does_not_publish_parser_hoisted_local_classes_globally() {
        let source = "fun local() { class Inner }\nfun other() = In";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&[source], &analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);

        assert!(index
            .complete(source, source.len() as u32)
            .iter()
            .all(|candidate| candidate.label != "Inner"));
    }

    #[test]
    fn completion_budget_marks_truncated_source_set_snapshots_incomplete() {
        let source = "fun answer(): Int = 42";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&[source], &analysis.files);
        let mut budget = CompletionBudget {
            entries: MAX_SOURCE_SET_COMPLETION_ENTRIES,
            wire_bytes: 0,
        };
        let index = CompletionIndex::from_file_analysis_with_budget(
            source,
            &analysis.files[0],
            &symbols,
            &mut budget,
        );

        assert!(index.is_incomplete());
        assert_eq!(index.entry_count(), 0);
    }

    #[test]
    fn semantic_tokens_match_official_kotlin_symbol_classification() {
        let source = concat!(
            "data class User(val name: String)\n",
            "fun greet(user: User): String = user.name"
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);

        assert_eq!(
            index.encode(None),
            vec![
                0, 11, 4, 4, 1, // data-class declaration: struct + declaration
                0, 9, 4, 7,
                5, // val constructor property declaration: parameter + declaration + readonly
                0, 6, 6, 1, 512, // kotlin.String: class + defaultLibrary
                1, 4, 5, 12, 9, // top-level function: function + declaration + static
                0, 6, 4, 7, 5, // value parameter: parameter + declaration + readonly
                0, 6, 4, 4, 0, // data-class type reference: struct
                0, 7, 6, 1, 512, // kotlin.String return: class + defaultLibrary
                0, 9, 4, 7, 4, // parameter reference: parameter + readonly
                0, 5, 4, 9, 4, // immutable property reference: property + readonly
            ]
        );
        assert_eq!(std::mem::size_of::<SemanticTokenEntry>(), 16);
    }

    #[test]
    fn semantic_token_range_encoding_rebases_the_first_token() {
        let source = "fun first() = 1\nfun second() = first()";
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);

        assert_eq!(
            index.encode(Some(SemanticTokenRange {
                start_line: 1,
                start_character: 0,
                end_line: 2,
                end_character: 0,
            })),
            vec![
                1, 4, 6, 12, 9, // second declaration
                0, 11, 5, 12, 8, // first reference
            ]
        );
    }

    #[test]
    fn semantic_tokens_match_official_constructor_and_enum_modifiers() {
        let source = concat!(
            "enum class Shade { RED }\n",
            "class Holder(var mutable: Int, val fixed: Int)\n",
            "fun paint(holder: Holder): Shade {\n",
            "holder.mutable = holder.fixed\n",
            "return Shade.RED\n",
            "}\n",
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let tokens = decoded_tokens(&index);

        assert!(tokens.contains(&(0, 19, 3, 10, 5))); // enum entry declaration: declaration + readonly
        assert!(tokens.contains(&(1, 17, 7, 7, 5))); // `var` property parameter: parameter + readonly
        assert!(tokens.contains(&(3, 7, 7, 9, 128))); // mutable property assignment
        assert!(tokens.contains(&(4, 13, 3, 10, 4))); // enum entry reference: readonly
    }

    #[test]
    fn semantic_token_range_includes_a_token_intersecting_its_start() {
        let source = "fun highlighted() = 1";
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);

        assert_eq!(
            index.encode(Some(SemanticTokenRange {
                start_line: 0,
                start_character: 8,
                end_line: 0,
                end_character: 10,
            })),
            vec![0, 4, 11, 12, 9]
        );
    }

    #[test]
    fn semantic_tokens_respect_lexical_shadowing_between_functions() {
        let source = concat!(
            "fun withParameter(item: Int) = item\n",
            "fun withLocal(): Int { val item = 1; return item }\n",
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let tokens = decoded_tokens(&index);

        assert!(tokens.contains(&(1, 44, 4, 8, 4)));
    }

    #[test]
    fn semantic_tokens_cover_alias_operator_deprecation_and_member_resolution() {
        let source = concat!(
            "typealias Label = String\n",
            "@Deprecated class Old\n",
            "class Box {\n",
            "  operator fun get(i: Int): Int = i\n",
            "  fun target(): Int = 1\n",
            "  fun caller(): Int = get(0) + target()\n",
            "}\n",
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let tokens = decoded_tokens(&index);

        assert!(tokens.contains(&(0, 10, 5, 1, 513))); // alias: expanded class + declaration + stdlib
        assert!(tokens.contains(&(1, 18, 3, 1, 17))); // deprecated class declaration
        assert!(tokens.contains(&(3, 15, 3, 21, 1))); // operator declaration
        assert!(tokens.contains(&(5, 22, 3, 21, 0))); // implicit-receiver operator call
        assert!(tokens.contains(&(5, 31, 6, 13, 0))); // implicit-receiver method call
    }

    #[test]
    fn semantic_tokens_resolve_the_terminal_import_symbol() {
        let source = "import kotlin.String\nfun echo(value: String) = value";
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let tokens = decoded_tokens(&index);

        assert!(tokens.contains(&(0, 7, 6, 0, 0))); // kotlin namespace
        assert!(tokens.contains(&(0, 14, 6, 1, 512))); // imported class, not a namespace
    }

    #[test]
    fn semantic_tokens_resolve_qualified_members_and_deprecated_references() {
        let source = concat!(
            "@Deprecated class Old\n",
            "enum class Color { RED }\n",
            "class A(val value: Int)\n",
            "class B(var value: Int)\n",
            "class Box { operator fun get(i: Int): Int = i; fun target(): Int = 1 }\n",
            "typealias Callback = (Int) -> Int\n",
            "fun inspect(a: A, b: B, box: Box): Int = a.value + b.value + box.get(0)\n",
            "fun reference() = Box::target\n",
            "fun color(): Color = Color.RED\n",
            "fun old(): Old = Old()\n",
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let tokens = decoded_tokens(&index);
        let lines: Vec<_> = source.lines().collect();

        assert!(tokens.contains(&(5, 10, 8, 3, 513))); // function alias: stdlib interface declaration
        assert!(tokens.contains(&(6, lines[6].find("value").unwrap() as u32, 5, 9, 4,))); // A.value: readonly
        assert!(tokens.contains(&(6, lines[6].find("b.value").unwrap() as u32 + 2, 5, 9, 128,))); // B.value: mutable
        assert!(tokens.contains(&(6, lines[6].find("get").unwrap() as u32, 3, 21, 0,))); // qualified operator call
        assert!(tokens.contains(&(7, lines[7].find("target").unwrap() as u32, 6, 13, 0)));
        assert!(tokens.contains(&(8, lines[8].find("RED").unwrap() as u32, 3, 10, 4)));
        assert!(tokens.contains(&(9, lines[9].find("Old").unwrap() as u32, 3, 1, 16)));
        assert!(tokens.contains(&(9, lines[9].rfind("Old").unwrap() as u32, 3, 1, 16)));
    }

    #[test]
    fn semantic_tokens_preserve_source_set_metadata_across_files() {
        let declaration = concat!(
            "@Deprecated data class Model(val value: Int)\n",
            "class Box { operator fun get(i: Int): Int = i }\n",
        );
        let usage = "fun use(model: Model, box: Box): Model { box.get(0); return model }";
        let sources = [declaration, usage];
        let analysis = analyze_standalone_source_set(&sources);
        let highlight_symbols =
            HighlightSymbols::from_source_set(&sources, &analysis.files, &analysis.symbols);
        let index = SemanticTokenIndex::from_source_set_file_analysis(
            usage,
            &analysis.files[1],
            &analysis.symbols,
            &highlight_symbols,
        );
        let tokens = decoded_tokens(&index);

        assert!(tokens.contains(&(0, 15, 5, 4, 16))); // cross-file data/deprecated parameter type
        assert!(tokens.contains(&(0, 33, 5, 4, 16))); // cross-file data/deprecated return type
        assert!(tokens.contains(&(0, 45, 3, 21, 0))); // cross-file operator member
    }

    #[test]
    fn semantic_token_worker_snapshot_uses_compact_array_entries() {
        let source = "fun answer(): Int = 42";
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let json = serde_json::to_value(&index).unwrap();

        assert!(json["entries"][0].is_array());
        assert_eq!(json["entries"][0].as_array().unwrap().len(), 4);
    }
}
