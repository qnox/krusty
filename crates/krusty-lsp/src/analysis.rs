//! Compact semantic data retained by interactive language-server queries.

use std::collections::{HashMap, HashSet};

use crate::compiler_analysis::{
    analyze_standalone_source_set, document_symbol_occurrences, folding_range_occurrences,
    hover_wire_cost, CompletionDetails, CompletionKind, CompletionSymbols, DefinitionOccurrence,
    DefinitionSymbols, DefinitionTarget, DocumentSymbolOccurrence, FileAnalysis,
    FoldingRangeOccurrence, FrontendSymbols, HighlightOccurrence, HighlightSymbols,
    HoverOccurrence, SemanticLimits, SignatureCandidate, SignatureHelpCall, SignatureHelpSymbols,
    FOLDING_KIND_COMMENT, FOLDING_KIND_IMPORTS, FOLDING_KIND_REGION, TEXT_BLOCK_COMMENT,
    TEXT_BRACES, TEXT_IMPORTS, TEXT_KDOC, TEXT_PARENTHESES, TEXT_RAW_STRING, TEXT_REGION_LABEL,
};
use krusty::diag::{Diagnostic, Span};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// `(source lo, source hi, interned hover value id)`.
type HoverEntry = [u32; 3];

/// Compact semantic snapshot retained for hover queries after full compiler analysis is dropped.
#[derive(Default, Deserialize, Serialize)]
pub struct HoverIndex {
    entries: Vec<HoverEntry>,
    values: Vec<String>,
}

pub struct Hover<'a> {
    pub span: Span,
    pub value: &'a str,
}

const NO_COMPLETION_TYPE: u32 = 0x003f_ffff;
const MAX_SOURCE_SET_COMPLETION_ENTRIES: usize = 32 * 1024;
const MAX_SOURCE_SET_COMPLETION_WIRE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_SOURCE_SET_NAVIGATION_ENTRIES: usize = 256 * 1024;
const MAX_SOURCE_SET_HOVER_ENTRIES: usize = 256 * 1024;
const MAX_SOURCE_SET_HOVER_WIRE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SOURCE_SET_DOCUMENT_SYMBOL_ENTRIES: usize = 32 * 1024;
const MAX_SOURCE_SET_DOCUMENT_SYMBOL_WIRE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_SET_FOLDING_RANGE_ENTRIES: usize = 32 * 1024;
const MAX_SOURCE_SET_FOLDING_RANGE_WIRE_BYTES: usize = 8 * 1024 * 1024;
const FOLDING_RANGE_WIRE_FIXED_BYTES: usize = 192;
const MAX_SOURCE_SET_SIGNATURE_HELP_CALLS: usize = 32 * 1024;
const MAX_SOURCE_SET_SIGNATURE_HELP_WIRE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct HoverBudget {
    entries: usize,
    wire_bytes: usize,
}

impl HoverBudget {
    fn remaining_entries(&self) -> usize {
        MAX_SOURCE_SET_HOVER_ENTRIES.saturating_sub(self.entries)
    }

    fn remaining_wire_bytes(&self) -> usize {
        MAX_SOURCE_SET_HOVER_WIRE_BYTES.saturating_sub(self.wire_bytes)
    }

    fn reserve(&mut self, value: &str, new_value: bool) -> bool {
        let bytes = hover_wire_cost(value, new_value);
        if self.entries >= MAX_SOURCE_SET_HOVER_ENTRIES || bytes > self.remaining_wire_bytes() {
            return false;
        }
        self.entries += 1;
        self.wire_bytes += bytes;
        true
    }
}

#[derive(Default)]
pub(crate) struct CompletionBudget {
    entries: usize,
    wire_bytes: usize,
}

#[derive(Default)]
pub(crate) struct NavigationBudget {
    entries: usize,
}

#[derive(Default)]
pub(crate) struct DocumentSymbolBudget {
    entries: usize,
    wire_bytes: usize,
}

#[derive(Default)]
pub(crate) struct FoldingRangeBudget {
    entries: usize,
    wire_bytes: usize,
}

#[derive(Default)]
pub(crate) struct SignatureHelpBudget {
    calls: usize,
    wire_bytes: usize,
}

impl SignatureHelpBudget {
    fn remaining_calls(&self) -> usize {
        MAX_SOURCE_SET_SIGNATURE_HELP_CALLS.saturating_sub(self.calls)
    }

    fn can_attempt(&self) -> bool {
        self.calls < MAX_SOURCE_SET_SIGNATURE_HELP_CALLS
            && self.wire_bytes < MAX_SOURCE_SET_SIGNATURE_HELP_WIRE_BYTES
    }

    fn remaining_argument_wire_bytes(&self) -> Option<usize> {
        MAX_SOURCE_SET_SIGNATURE_HELP_WIRE_BYTES
            .saturating_sub(self.wire_bytes)
            .checked_sub(96)
    }

    fn reserve(
        &mut self,
        call: &SignatureHelpCall,
        candidates: &[SignatureCandidate],
        new_group: bool,
    ) -> bool {
        let argument_bytes = call.arguments.iter().fold(0usize, |bytes, argument| {
            bytes.saturating_add(argument.wire_bytes())
        });
        let signature_bytes = if new_group {
            candidates.iter().fold(0usize, |bytes, candidate| {
                bytes
                    .saturating_add(32)
                    .saturating_add(candidate.label.len().saturating_mul(6))
                    .saturating_add(candidate.parameters.iter().fold(
                        0usize,
                        |parameter_bytes, parameter| {
                            parameter_bytes
                                .saturating_add(24)
                                .saturating_add(parameter.name.len().saturating_mul(6))
                        },
                    ))
            })
        } else {
            0
        };
        let wire_bytes = 96usize
            .saturating_add(argument_bytes)
            .saturating_add(signature_bytes);
        if self.calls >= MAX_SOURCE_SET_SIGNATURE_HELP_CALLS
            || wire_bytes > MAX_SOURCE_SET_SIGNATURE_HELP_WIRE_BYTES.saturating_sub(self.wire_bytes)
        {
            return false;
        }
        self.calls += 1;
        self.wire_bytes += wire_bytes;
        true
    }
}

impl DocumentSymbolBudget {
    fn remaining_entries(&self) -> usize {
        MAX_SOURCE_SET_DOCUMENT_SYMBOL_ENTRIES.saturating_sub(self.entries)
    }

    fn reserve(&mut self, name: &str) -> bool {
        let wire_bytes = 192usize.saturating_add(name.len().saturating_mul(6));
        if self.entries >= MAX_SOURCE_SET_DOCUMENT_SYMBOL_ENTRIES
            || wire_bytes
                > MAX_SOURCE_SET_DOCUMENT_SYMBOL_WIRE_BYTES.saturating_sub(self.wire_bytes)
        {
            return false;
        }
        self.entries += 1;
        self.wire_bytes += wire_bytes;
        true
    }
}

impl FoldingRangeBudget {
    fn remaining_entries(&self) -> usize {
        MAX_SOURCE_SET_FOLDING_RANGE_ENTRIES.saturating_sub(self.entries)
    }

    fn reserve(&mut self, collapsed_text_bytes: usize) -> bool {
        let wire_bytes =
            FOLDING_RANGE_WIRE_FIXED_BYTES.saturating_add(collapsed_text_bytes.saturating_mul(6));
        if self.entries >= MAX_SOURCE_SET_FOLDING_RANGE_ENTRIES
            || wire_bytes > MAX_SOURCE_SET_FOLDING_RANGE_WIRE_BYTES.saturating_sub(self.wire_bytes)
        {
            return false;
        }
        self.entries += 1;
        self.wire_bytes += wire_bytes;
        true
    }
}

impl NavigationBudget {
    fn remaining(&self) -> usize {
        MAX_SOURCE_SET_NAVIGATION_ENTRIES.saturating_sub(self.entries)
    }
}

impl CompletionBudget {
    fn reserve(
        &mut self,
        label: &str,
        details: &CompletionDetails,
        result_type: Option<&str>,
    ) -> bool {
        let string_bytes = label
            .len()
            .saturating_add(details.detail.len())
            .saturating_add(details.description.len())
            .saturating_add(1)
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
}

pub struct Completion<'a> {
    pub label: &'a str,
    pub kind: u8,
    pub label_detail: Option<&'a str>,
    pub label_description: Option<&'a str>,
}

/// `(source lo, source hi, target file, target lo, target hi)`.
type DefinitionEntry = [u32; 5];

#[derive(Default, Deserialize, Serialize)]
pub struct DefinitionIndex {
    entries: Vec<DefinitionEntry>,
}

/// `(start line, start UTF-16 column, end line, end UTF-16 column,
/// kind + collapsed-text style, summary source byte lo, summary source byte hi)`.
type FoldingRangeEntry = [u32; 7];

#[derive(Default, Deserialize, Serialize)]
pub struct FoldingRangeIndex {
    entries: Vec<FoldingRangeEntry>,
}

/// `(name id, range start line/character, range end line/character,
/// selection start line/character, selection end line/character, kind/deprecated/parent)`.
type DocumentSymbolEntry = [u32; 10];

/// Compact pre-positioned hierarchy retained after compiler analysis is dropped.
#[derive(Default, Deserialize, Serialize)]
pub struct DocumentSymbolIndex {
    entries: Vec<DocumentSymbolEntry>,
    names: Vec<String>,
}

/// `(argument-list lo, hi, signature start/count, selected signature, argument start/count,
/// optional containing-call index + 1)`.
type SignatureHelpCallEntry = [u32; 8];
/// `(label string id, parameter start, parameter count)`.
type SignatureHelpSignatureEntry = [u32; 3];
/// `(name string id, label UTF-16 start, label UTF-16 end)`.
type SignatureHelpParameterEntry = [u32; 3];
/// `(argument end byte, optional name string id + 1)`.
type SignatureHelpArgumentEntry = [u32; 2];
const SIGNATURE_HELP_VARARG_BIT: u32 = 1 << 31;

/// Compact call ranges and signature labels retained after compiler analysis is dropped.
#[derive(Default, Deserialize, Serialize)]
pub struct SignatureHelpIndex {
    calls: Vec<SignatureHelpCallEntry>,
    signatures: Vec<SignatureHelpSignatureEntry>,
    parameters: Vec<SignatureHelpParameterEntry>,
    arguments: Vec<SignatureHelpArgumentEntry>,
    strings: Vec<String>,
}

impl SignatureHelpIndex {
    fn from_file_analysis(
        source: &str,
        analysis: &FileAnalysis,
        symbols: &SignatureHelpSymbols,
        frontend_symbols: &FrontendSymbols,
        budget: &mut SignatureHelpBudget,
    ) -> Self {
        let mut result = Self::default();
        if !budget.can_attempt() || budget.remaining_argument_wire_bytes().is_none() {
            return result;
        }
        let call_sites = symbols.call_sites(source, analysis, budget.remaining_calls());
        let mut strings = HashMap::<String, u32>::new();
        let mut groups = HashMap::<usize, (u32, u32)>::new();
        let mut containing_calls = Vec::<usize>::new();

        for site in call_sites {
            if !budget.can_attempt() {
                break;
            }
            let Some(argument_wire_bytes) = budget.remaining_argument_wire_bytes() else {
                break;
            };
            let call = match symbols.call(
                source,
                analysis,
                frontend_symbols,
                site,
                argument_wire_bytes,
            ) {
                Ok(Some(call)) => call,
                Ok(None) => continue,
                Err(()) => break,
            };
            let candidates = symbols.candidates_for_call(source, analysis, frontend_symbols, &call);
            let candidates = candidates.as_ref();
            let share_group = SignatureHelpSymbols::call_shares_group(&call);
            let new_group = !share_group || !groups.contains_key(&call.group);
            if !budget.reserve(&call, candidates, new_group) {
                break;
            }
            let shared_group = share_group
                .then(|| groups.get(&call.group).copied())
                .flatten();
            let (signature_start, signature_count) = if let Some(group) = shared_group {
                group
            } else {
                let signature_start = result.signatures.len() as u32;
                for candidate in candidates {
                    let label = intern_signature_string(
                        &candidate.label,
                        &mut result.strings,
                        &mut strings,
                    );
                    let parameter_start = result.parameters.len() as u32;
                    for parameter in &candidate.parameters {
                        let name = intern_signature_string(
                            &parameter.name,
                            &mut result.strings,
                            &mut strings,
                        );
                        result
                            .parameters
                            .push([name, parameter.label_start, parameter.label_end]);
                    }
                    result.signatures.push([
                        label,
                        parameter_start,
                        candidate.parameters.len() as u32
                            | if candidate.is_vararg() {
                                SIGNATURE_HELP_VARARG_BIT
                            } else {
                                0
                            },
                    ]);
                }
                let group = (signature_start, candidates.len() as u32);
                if share_group {
                    groups.insert(call.group, group);
                }
                group
            };
            let argument_start = result.arguments.len() as u32;
            for argument in &call.arguments {
                let name = argument.name.as_ref().map_or(0, |name| {
                    intern_signature_string(name, &mut result.strings, &mut strings)
                        .saturating_add(1)
                });
                result.arguments.push([argument.end, name]);
            }
            while containing_calls.last().is_some_and(|parent| {
                let parent = result.calls[*parent];
                parent[0] > call.span.lo || parent[1] < call.span.hi
            }) {
                containing_calls.pop();
            }
            let parent = containing_calls
                .last()
                .map_or(0, |parent| (*parent as u32).saturating_add(1));
            let call_index = result.calls.len();
            result.calls.push([
                call.span.lo,
                call.span.hi,
                signature_start,
                signature_count,
                call.selected as u32,
                argument_start,
                call.arguments.len() as u32,
                parent,
            ]);
            containing_calls.push(call_index);
        }
        result
    }

    pub fn encode(&self, offset: u32) -> Option<Value> {
        let mut call_index = self.calls.partition_point(|call| call[0] <= offset);
        let call = loop {
            call_index = call_index.checked_sub(1)?;
            let call = &self.calls[call_index];
            if offset <= call[1] {
                break call;
            }
            call_index = call[7] as usize;
        };
        let arguments = &self.arguments[call[5] as usize..call[5].saturating_add(call[6]) as usize];
        let active_argument = arguments
            .partition_point(|argument| argument[0] < offset)
            .min(arguments.len().saturating_sub(1));
        let active_name = arguments
            .get(active_argument)
            .and_then(|argument| argument[1].checked_sub(1));
        let signatures =
            &self.signatures[call[2] as usize..call[2].saturating_add(call[3]) as usize];
        let signatures = signatures
            .iter()
            .map(|signature| {
                let parameter_count = signature[2] & !SIGNATURE_HELP_VARARG_BIT;
                let is_vararg = signature[2] & SIGNATURE_HELP_VARARG_BIT != 0;
                let parameters = &self.parameters
                    [signature[1] as usize..signature[1].saturating_add(parameter_count) as usize];
                let matching_parameter = active_name
                    .and_then(|name| parameters.iter().position(|parameter| parameter[0] == name));
                let missing_named_parameter =
                    active_name.is_some() && matching_parameter.is_none() && !parameters.is_empty();
                let active_parameter = if parameters.is_empty()
                    || (active_argument >= parameters.len() && !is_vararg)
                {
                    None
                } else if missing_named_parameter {
                    Some((active_argument + 1).min(parameters.len()))
                } else {
                    Some(matching_parameter.unwrap_or(active_argument.min(parameters.len() - 1)))
                };
                let mut encoded_parameters = parameters
                    .iter()
                    .map(|parameter| json!({"label": [parameter[1], parameter[2]]}))
                    .collect::<Vec<_>>();
                if missing_named_parameter {
                    let insertion = active_parameter.unwrap();
                    let offset = if insertion == 0 {
                        parameters[0][1].saturating_sub(1)
                    } else {
                        parameters[insertion - 1][2].saturating_add(1)
                    };
                    encoded_parameters.insert(insertion, json!({"label": [offset, offset]}));
                }
                let mut encoded = json!({
                    "label": self.strings[signature[0] as usize],
                    "parameters": encoded_parameters,
                });
                if let Some(active_parameter) = active_parameter {
                    encoded
                        .as_object_mut()
                        .unwrap()
                        .insert("activeParameter".to_string(), json!(active_parameter));
                }
                encoded
            })
            .collect::<Vec<_>>();
        Some(json!({
            "signatures": signatures,
            "activeSignature": (call[4] as usize).min(signatures.len().saturating_sub(1)),
        }))
    }

    pub fn entry_count(&self) -> usize {
        self.calls.len()
    }
}

fn intern_signature_string(
    value: &str,
    strings: &mut Vec<String>,
    ids: &mut HashMap<String, u32>,
) -> u32 {
    if let Some(&id) = ids.get(value) {
        return id;
    }
    let id = strings.len() as u32;
    let value = value.to_string();
    ids.insert(value.clone(), id);
    strings.push(value);
    id
}

impl DocumentSymbolIndex {
    fn from_occurrences(
        source: &str,
        occurrences: Vec<DocumentSymbolOccurrence>,
        budget: &mut DocumentSymbolBudget,
    ) -> Self {
        let mut retained = Vec::with_capacity(
            occurrences
                .len()
                .min(MAX_SOURCE_SET_DOCUMENT_SYMBOL_ENTRIES.saturating_sub(budget.entries)),
        );
        for occurrence in occurrences {
            if !budget.reserve(&occurrence.name) {
                break;
            }
            retained.push(occurrence);
        }

        let positions = selected_positions(
            source,
            retained.iter().flat_map(|occurrence| {
                [
                    occurrence.range.lo,
                    occurrence.range.hi,
                    occurrence.selection.lo,
                    occurrence.selection.hi,
                ]
            }),
        );
        let position = |offset| {
            let index = positions
                .binary_search_by_key(&offset, |(offset, _)| *offset)
                .expect("document-symbol offset must be positioned");
            positions[index].1
        };

        let mut names = Vec::new();
        let mut name_ids = HashMap::<String, u32>::new();
        let mut entries = Vec::with_capacity(retained.len());
        for occurrence in retained {
            let name_id = if let Some(&name_id) = name_ids.get(&occurrence.name) {
                name_id
            } else {
                let name_id = names.len() as u32;
                name_ids.insert(occurrence.name.clone(), name_id);
                names.push(occurrence.name);
                name_id
            };
            let range_start = position(occurrence.range.lo);
            let range_end = position(occurrence.range.hi);
            let selection_start = position(occurrence.selection.lo);
            let selection_end = position(occurrence.selection.hi);
            let parent = occurrence
                .parent
                .and_then(|parent| u32::try_from(parent).ok())
                .and_then(|parent| parent.checked_add(1))
                .unwrap_or(0);
            let packed =
                u32::from(occurrence.kind) | u32::from(occurrence.deprecated) << 8 | parent << 9;
            entries.push([
                name_id,
                range_start[0],
                range_start[1],
                range_end[0],
                range_end[1],
                selection_start[0],
                selection_start[1],
                selection_end[0],
                selection_end[1],
                packed,
            ]);
        }
        Self { entries, names }
    }

    pub fn encode(&self) -> Vec<Value> {
        let mut children = (0..self.entries.len())
            .map(|_| Vec::new())
            .collect::<Vec<Vec<Value>>>();
        let mut roots = Vec::new();
        for index in (0..self.entries.len()).rev() {
            let entry = self.entries[index];
            let packed = entry[9];
            let mut symbol = json!({
                "name": self.names[entry[0] as usize],
                "kind": packed & u8::MAX as u32,
                "deprecated": packed & (1 << 8) != 0,
                "range": {
                    "start": {"line": entry[1], "character": entry[2]},
                    "end": {"line": entry[3], "character": entry[4]},
                },
                "selectionRange": {
                    "start": {"line": entry[5], "character": entry[6]},
                    "end": {"line": entry[7], "character": entry[8]},
                }
            });
            if packed & (1 << 8) != 0 {
                symbol
                    .as_object_mut()
                    .unwrap()
                    .insert("tags".to_string(), json!([1]));
            }
            let symbol_children = &mut children[index];
            if !symbol_children.is_empty() {
                symbol_children.reverse();
                symbol.as_object_mut().unwrap().insert(
                    "children".to_string(),
                    Value::Array(std::mem::take(symbol_children)),
                );
            }
            let parent = packed >> 9;
            if parent == 0 {
                roots.push(symbol);
            } else {
                children[parent as usize - 1].push(symbol);
            }
        }
        roots.reverse();
        roots
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn name_count(&self) -> usize {
        self.names.len()
    }
}

fn selected_positions(
    source: &str,
    offsets: impl IntoIterator<Item = u32>,
) -> Vec<(u32, [u32; 2])> {
    let mut offsets = offsets.into_iter().collect::<Vec<_>>();
    offsets.sort_unstable();
    offsets.dedup();
    let mut positions = Vec::with_capacity(offsets.len());
    let mut byte = 0usize;
    let mut line = 0u32;
    let mut character = 0u32;
    let mut previous_was_cr = false;
    for offset in offsets {
        let offset = offset as usize;
        advance_position(
            &source[byte..offset],
            &mut line,
            &mut character,
            &mut previous_was_cr,
        );
        positions.push((offset as u32, [line, character]));
        byte = offset;
    }
    positions
}

impl FoldingRangeIndex {
    fn from_occurrences(
        source: &str,
        occurrences: Vec<FoldingRangeOccurrence>,
        budget: &mut FoldingRangeBudget,
    ) -> Self {
        let positions = selected_positions(
            source,
            occurrences
                .iter()
                .flat_map(|occurrence| [occurrence.span.lo, occurrence.span.hi]),
        );
        let position = |offset| {
            let index = positions
                .binary_search_by_key(&offset, |(offset, _)| *offset)
                .expect("folding-range offset must be positioned");
            positions[index].1
        };

        let mut entries = Vec::with_capacity(
            occurrences
                .len()
                .min(MAX_SOURCE_SET_FOLDING_RANGE_ENTRIES.saturating_sub(budget.entries)),
        );
        for occurrence in occurrences {
            let start = position(occurrence.span.lo);
            let end = position(occurrence.span.hi);
            if start[0] >= end[0] {
                continue;
            }
            if !budget.reserve(occurrence.text.collapsed_text_bytes()) {
                break;
            }
            let summary = occurrence.text.summary();
            entries.push([
                start[0],
                start[1],
                end[0],
                end[1],
                u32::from(occurrence.kind) << 8 | u32::from(occurrence.text.style()),
                summary.lo,
                summary.hi,
            ]);
        }
        Self { entries }
    }

    pub fn encode(&self, source: &str) -> Vec<Value> {
        self.entries
            .iter()
            .map(|entry| {
                let packed = entry[4];
                let kind = match (packed >> 8) as u8 {
                    FOLDING_KIND_COMMENT => "comment",
                    FOLDING_KIND_IMPORTS => "imports",
                    FOLDING_KIND_REGION => "region",
                    _ => "region",
                };
                let summary = source
                    .get(entry[5] as usize..entry[6] as usize)
                    .unwrap_or("");
                let collapsed_text = match packed as u8 {
                    TEXT_IMPORTS => "...".to_string(),
                    TEXT_PARENTHESES => "(...)".to_string(),
                    TEXT_BRACES => "{...}".to_string(),
                    TEXT_KDOC => format!("/** {summary} ...*/"),
                    TEXT_BLOCK_COMMENT => format!("/ {summary} .../"),
                    TEXT_RAW_STRING => format!("\"\"\"{summary} ...\"\"\""),
                    TEXT_REGION_LABEL => summary.to_string(),
                    _ => String::new(),
                };
                json!({
                    "startLine": entry[0],
                    "startCharacter": entry[1],
                    "endLine": entry[2],
                    "endCharacter": entry[3],
                    "kind": kind,
                    "collapsedText": collapsed_text,
                })
            })
            .collect()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
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
    #[cfg(test)]
    pub(crate) fn wire_saturation_fixture(entry_count: usize) -> Self {
        Self {
            entries: vec![[u32::MAX; 5]; entry_count],
        }
    }

    fn build(occurrences: Vec<DefinitionOccurrence>, available: usize) -> Self {
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
        Self { entries }
    }

    fn from_occurrences(
        occurrences: Vec<DefinitionOccurrence>,
        budget: &mut NavigationBudget,
    ) -> Self {
        let available = budget.remaining();
        let index = Self::build(occurrences, available);
        budget.entries += index.entries.len();
        index
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

    /// Occurrences that resolve to one of the selected declarations.
    pub fn occurrences_targeting<'a>(
        &'a self,
        targets: &'a HashSet<DefinitionTarget>,
    ) -> impl Iterator<Item = (Span, DefinitionTarget)> + 'a {
        self.entries
            .iter()
            .map(|entry| {
                (
                    Span::new(entry[0], entry[1]),
                    DefinitionTarget {
                        file: entry[2],
                        span: Span::new(entry[3], entry[4]),
                    },
                )
            })
            .filter(move |(_, target)| targets.contains(target))
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
        let entries = scoped
            .into_iter()
            .filter_map(|symbol| {
                if !budget.reserve(
                    &symbol.label,
                    &symbol.details,
                    symbol.result_type.as_deref(),
                ) {
                    return None;
                }
                let label = intern(&symbol.label);
                let label_details = intern(&pack_completion_details(&symbol.details));
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
                    label_details,
                    symbol.kind as u32 | result_type << 8 | u32::from(symbol.priority) << 30,
                ])
            })
            .collect();
        let members = symbols
            .members()
            .filter(|(owner, _, _, _)| member_owners.contains(*owner))
            .filter_map(|(owner, label, details, kind)| {
                if !budget.reserve(label, details, Some(owner)) {
                    return None;
                }
                Some([
                    intern(owner),
                    intern(label),
                    intern(&pack_completion_details(details)),
                    kind as u32,
                ])
            })
            .collect();
        Self {
            entries,
            members,
            strings,
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
                .map(|(index, entry)| {
                    let (label_detail, label_description) =
                        completion_label_details(&self.strings[entry[2] as usize]);
                    (
                        index,
                        Completion {
                            label: &self.strings[entry[1] as usize],
                            kind: entry[3] as u8,
                            label_detail,
                            label_description,
                        },
                    )
                })
                .collect();
            result.sort_unstable_by_key(|(index, candidate)| {
                (completion_kind_group(candidate.kind), *index)
            });
            let mut seen = HashSet::new();
            result.retain(|(_, candidate)| seen.insert(candidate.label));
            return result.into_iter().map(|(_, candidate)| candidate).collect();
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
            .map(|(label, (index, width, priority))| {
                let entry = &self.entries[index];
                let (label_detail, label_description) =
                    completion_label_details(&self.strings[entry[4] as usize]);
                (
                    Completion {
                        label,
                        kind: entry[5] as u8,
                        label_detail,
                        label_description,
                    },
                    width,
                    priority,
                    index,
                )
            })
            .collect();
        result.sort_unstable_by_key(|(candidate, width, priority, index)| {
            (
                completion_kind_group(candidate.kind),
                std::cmp::Reverse(*priority),
                *width,
                candidate.label,
                *index,
            )
        });
        result
            .into_iter()
            .map(|(candidate, _, _, _)| candidate)
            .collect()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

fn completion_label_details(value: &str) -> (Option<&str>, Option<&str>) {
    let (detail, description) = value.split_once('\0').unwrap_or((value, ""));
    (
        (!detail.is_empty()).then_some(detail),
        (!description.is_empty()).then_some(description),
    )
}

fn completion_kind_group(kind: u8) -> u8 {
    if [
        CompletionKind::Variable,
        CompletionKind::Property,
        CompletionKind::EnumMember,
        CompletionKind::Constant,
    ]
    .iter()
    .any(|candidate| *candidate as u8 == kind)
    {
        0
    } else if [
        CompletionKind::Method,
        CompletionKind::Function,
        CompletionKind::Operator,
    ]
    .iter()
    .any(|candidate| *candidate as u8 == kind)
    {
        1
    } else {
        2
    }
}

fn pack_completion_details(details: &CompletionDetails) -> String {
    format!("{}\0{}", details.detail, details.description)
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
            HighlightSymbols::from_source_set(std::slice::from_ref(analysis), symbols);
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
    fn from_occurrences(rich_occurrences: Vec<HoverOccurrence>, budget: &mut HoverBudget) -> Self {
        let capacity = rich_occurrences.len().min(budget.remaining_entries());
        let mut values = Vec::new();
        let mut value_indices = HashMap::<String, u32>::new();
        let mut entries = Vec::with_capacity(capacity);
        let mut entry_keys = HashSet::with_capacity(capacity);

        for occurrence in rich_occurrences {
            if entries.len() >= capacity {
                break;
            }
            let existing_value_index = value_indices.get(&occurrence.value).copied();
            let value_index = existing_value_index.unwrap_or(values.len() as u32);
            let entry = [occurrence.span.lo, occurrence.span.hi, value_index];
            if !entry_keys.insert(entry) {
                continue;
            }
            let new_value = existing_value_index.is_none();
            if !budget.reserve(&occurrence.value, new_value) {
                break;
            }
            if new_value {
                value_indices.insert(occurrence.value.clone(), value_index);
                values.push(occurrence.value);
            }
            entries.push(entry);
        }
        entries.sort_unstable();
        Self { entries, values }
    }

    pub fn get(&self, byte_offset: u32) -> Option<Hover<'_>> {
        self.entries
            .iter()
            .filter(|entry| {
                entry[0] <= byte_offset
                    && (byte_offset < entry[1] || (entry[0] == entry[1] && byte_offset == entry[0]))
            })
            .min_by_key(|entry| entry[1].saturating_sub(entry[0]))
            .map(|entry| Hover {
                span: Span::new(entry[0], entry[1]),
                value: &self.values[entry[2] as usize],
            })
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn value_count(&self) -> usize {
        self.values.len()
    }
}

pub struct DocumentAnalysis {
    pub diagnostics: Vec<Diagnostic>,
    pub hover: HoverIndex,
    pub completion: CompletionIndex,
    pub signature_help: SignatureHelpIndex,
    pub semantic_tokens: SemanticTokenIndex,
    pub definitions: DefinitionIndex,
    pub type_definitions: DefinitionIndex,
    pub document_symbols: DocumentSymbolIndex,
    pub folding_ranges: FoldingRangeIndex,
}

pub(crate) struct SourceSetIndexes<'a> {
    symbols: &'a FrontendSymbols,
    highlights: &'a HighlightSymbols,
    definitions: &'a DefinitionSymbols,
    completions: &'a CompletionSymbols,
    signatures: &'a SignatureHelpSymbols,
}

impl<'a> SourceSetIndexes<'a> {
    pub(crate) fn new(
        symbols: &'a FrontendSymbols,
        highlights: &'a HighlightSymbols,
        definitions: &'a DefinitionSymbols,
        completions: &'a CompletionSymbols,
        signatures: &'a SignatureHelpSymbols,
    ) -> Self {
        Self {
            symbols,
            highlights,
            definitions,
            completions,
            signatures,
        }
    }
}

pub(crate) struct AnalysisBudgets {
    hover: HoverBudget,
    completion: CompletionBudget,
    navigation: NavigationBudget,
    pending_type_definitions: usize,
    document_symbol: DocumentSymbolBudget,
    folding_range: FoldingRangeBudget,
    signature_help: SignatureHelpBudget,
}

impl AnalysisBudgets {
    pub(crate) fn new() -> Self {
        Self {
            hover: HoverBudget::default(),
            completion: CompletionBudget::default(),
            navigation: NavigationBudget::default(),
            pending_type_definitions: 0,
            document_symbol: DocumentSymbolBudget::default(),
            folding_range: FoldingRangeBudget::default(),
            signature_help: SignatureHelpBudget::default(),
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
    ) -> (Self, Vec<DefinitionOccurrence>) {
        let completion = CompletionIndex::from_file_analysis_with_budget(
            source,
            &analysis,
            indexes.completions,
            &mut budgets.completion,
        );
        let signature_help = SignatureHelpIndex::from_file_analysis(
            source,
            &analysis,
            indexes.signatures,
            indexes.symbols,
            &mut budgets.signature_help,
        );
        let semantic = analysis.semantic_occurrences(
            source,
            file_index,
            indexes.symbols,
            indexes.highlights,
            indexes.definitions,
            SemanticLimits {
                definition_entries: budgets.navigation.remaining(),
                type_definition_entries: budgets.navigation.remaining().min(
                    MAX_SOURCE_SET_NAVIGATION_ENTRIES
                        .saturating_sub(budgets.pending_type_definitions),
                ),
                hover_entries: budgets.hover.remaining_entries(),
                hover_wire_bytes: budgets.hover.remaining_wire_bytes(),
            },
        );
        budgets.pending_type_definitions += semantic.type_definitions.len();
        let hover = HoverIndex::from_occurrences(semantic.hovers, &mut budgets.hover);
        let semantic_tokens = SemanticTokenIndex::from_occurrences(source, semantic.highlights);
        let definitions =
            DefinitionIndex::from_occurrences(semantic.definitions, &mut budgets.navigation);
        let document_symbols = DocumentSymbolIndex::from_occurrences(
            source,
            document_symbol_occurrences(
                source,
                &analysis,
                budgets.document_symbol.remaining_entries(),
            ),
            &mut budgets.document_symbol,
        );
        let folding_ranges = FoldingRangeIndex::from_occurrences(
            source,
            folding_range_occurrences(source, &analysis, budgets.folding_range.remaining_entries()),
            &mut budgets.folding_range,
        );
        (
            Self {
                diagnostics: analysis.diagnostics,
                hover,
                completion,
                signature_help,
                semantic_tokens,
                definitions,
                type_definitions: DefinitionIndex::default(),
                document_symbols,
                folding_ranges,
            },
            semantic.type_definitions,
        )
    }

    pub fn with_diagnostics(diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            diagnostics,
            hover: HoverIndex::default(),
            completion: CompletionIndex::default(),
            signature_help: SignatureHelpIndex::default(),
            semantic_tokens: SemanticTokenIndex::default(),
            definitions: DefinitionIndex::default(),
            type_definitions: DefinitionIndex::default(),
            document_symbols: DocumentSymbolIndex::default(),
            folding_ranges: FoldingRangeIndex::default(),
        }
    }

    pub fn empty() -> Self {
        Self::with_diagnostics(Vec::new())
    }
}

pub(crate) fn finalize_type_definitions(
    pending: Vec<(DocumentAnalysis, Vec<DefinitionOccurrence>)>,
    budgets: &mut AnalysisBudgets,
) -> Vec<DocumentAnalysis> {
    pending
        .into_iter()
        .map(|(mut analysis, occurrences)| {
            analysis.type_definitions =
                DefinitionIndex::from_occurrences(occurrences, &mut budgets.navigation);
            analysis
        })
        .collect()
}

/// Analyze one source in an open source set and retain only data needed by editor queries.
pub fn analyze_for_lsp(sources: &[&str]) -> Vec<DocumentAnalysis> {
    let analysis = analyze_standalone_source_set(sources);
    let highlight_symbols = HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
    let definition_symbols =
        DefinitionSymbols::from_source_set(sources, &analysis.files, &analysis.symbols);
    let completion_symbols = CompletionSymbols::from_source_set(&analysis.files);
    let signature_help_symbols =
        SignatureHelpSymbols::from_source_set(sources, &analysis.files, &analysis.symbols);
    let indexes = SourceSetIndexes::new(
        &analysis.symbols,
        &highlight_symbols,
        &definition_symbols,
        &completion_symbols,
        &signature_help_symbols,
    );
    let mut budgets = AnalysisBudgets::new();
    let pending = analysis
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
        .collect();
    finalize_type_definitions(pending, &mut budgets)
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
    fn document_symbol_snapshot_is_compact_interned_hierarchical_and_utf16_positioned() {
        assert_eq!(std::mem::size_of::<DocumentSymbolEntry>(), 40);
        let source = "😀\r\nx";
        let occurrences = vec![
            DocumentSymbolOccurrence {
                name: "same".to_string(),
                kind: 5,
                deprecated: false,
                range: Span::new(0, source.len() as u32),
                selection: Span::new(6, 7),
                parent: None,
            },
            DocumentSymbolOccurrence {
                name: "same".to_string(),
                kind: 7,
                deprecated: true,
                range: Span::new(6, 7),
                selection: Span::new(6, 7),
                parent: Some(0),
            },
        ];
        let index = DocumentSymbolIndex::from_occurrences(
            source,
            occurrences,
            &mut DocumentSymbolBudget::default(),
        );

        assert_eq!(index.entry_count(), 2);
        assert_eq!(index.name_count(), 1);
        assert_eq!(
            index.encode(),
            vec![json!({
                "name": "same",
                "kind": 5,
                "deprecated": false,
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 1, "character": 1}
                },
                "selectionRange": {
                    "start": {"line": 1, "character": 0},
                    "end": {"line": 1, "character": 1}
                },
                "children": [{
                    "name": "same",
                    "kind": 7,
                    "deprecated": true,
                    "tags": [1],
                    "range": {
                        "start": {"line": 1, "character": 0},
                        "end": {"line": 1, "character": 1}
                    },
                    "selectionRange": {
                        "start": {"line": 1, "character": 0},
                        "end": {"line": 1, "character": 1}
                    }
                }]
            })]
        );
    }

    #[test]
    fn document_symbol_snapshot_respects_the_source_set_entry_budget() {
        let source = "x";
        let occurrences = (0..=MAX_SOURCE_SET_DOCUMENT_SYMBOL_ENTRIES)
            .map(|_| DocumentSymbolOccurrence {
                name: "x".to_string(),
                kind: 13,
                deprecated: false,
                range: Span::new(0, 1),
                selection: Span::new(0, 1),
                parent: None,
            })
            .collect();
        let index = DocumentSymbolIndex::from_occurrences(
            source,
            occurrences,
            &mut DocumentSymbolBudget::default(),
        );

        assert_eq!(index.entry_count(), MAX_SOURCE_SET_DOCUMENT_SYMBOL_ENTRIES);
        assert_eq!(index.name_count(), 1);
    }

    #[test]
    fn folding_range_snapshot_is_compact_source_referenced_and_utf16_positioned() {
        use crate::compiler_analysis::{FoldingRangeOccurrence, FoldingRangeText};

        assert_eq!(std::mem::size_of::<FoldingRangeEntry>(), 28);
        let source = "same\n😀(\r\nx\r\n)";
        let range = Span::new(source.find('(').unwrap() as u32, source.len() as u32);
        let index = FoldingRangeIndex::from_occurrences(
            source,
            vec![
                FoldingRangeOccurrence {
                    span: range,
                    kind: FOLDING_KIND_REGION,
                    text: FoldingRangeText::RegionLabel(Span::new(0, 4)),
                },
                FoldingRangeOccurrence {
                    span: range,
                    kind: FOLDING_KIND_COMMENT,
                    text: FoldingRangeText::RegionLabel(Span::new(0, 4)),
                },
            ],
            &mut FoldingRangeBudget::default(),
        );

        assert_eq!(index.entry_count(), 2);
        assert_eq!(
            serde_json::to_value(&index).unwrap(),
            json!({"entries": [
                [1, 2, 3, 1, 518, 0, 4],
                [1, 2, 3, 1, 6, 0, 4]
            ]})
        );
        assert_eq!(
            index.encode(source),
            vec![
                json!({
                    "startLine": 1,
                    "startCharacter": 2,
                    "endLine": 3,
                    "endCharacter": 1,
                    "kind": "region",
                    "collapsedText": "same"
                }),
                json!({
                    "startLine": 1,
                    "startCharacter": 2,
                    "endLine": 3,
                    "endCharacter": 1,
                    "kind": "comment",
                    "collapsedText": "same"
                })
            ]
        );
    }

    #[test]
    fn folding_range_snapshot_is_source_set_and_expanded_wire_bounded() {
        use crate::compiler_analysis::{FoldingRangeOccurrence, FoldingRangeText};

        let source = "(\n)";
        let same_line = FoldingRangeIndex::from_occurrences(
            source,
            vec![FoldingRangeOccurrence {
                span: Span::new(0, 1),
                kind: FOLDING_KIND_REGION,
                text: FoldingRangeText::Braces,
            }],
            &mut FoldingRangeBudget::default(),
        );
        assert_eq!(same_line.entry_count(), 0);

        let large = "m".repeat(4 * 1024);
        let source = format!("{large}\n(\n)");
        let fold_start = source.find('(').unwrap() as u32;
        let occurrences = (0..3000)
            .map(|_| FoldingRangeOccurrence {
                span: Span::new(fold_start, source.len() as u32),
                kind: FOLDING_KIND_COMMENT,
                text: FoldingRangeText::RegionLabel(Span::new(0, large.len() as u32)),
            })
            .collect();
        let mut budget = FoldingRangeBudget::default();
        let bounded = FoldingRangeIndex::from_occurrences(&source, occurrences, &mut budget);
        assert!(!bounded.entries.is_empty());
        assert!(bounded.entries.len() < 3000);
        assert!(budget.wire_bytes <= MAX_SOURCE_SET_FOLDING_RANGE_WIRE_BYTES);
        assert!(
            serde_json::to_vec(&Value::Array(bounded.encode(&source)))
                .unwrap()
                .len()
                <= MAX_SOURCE_SET_FOLDING_RANGE_WIRE_BYTES
        );

        for mut exhausted in [
            FoldingRangeBudget {
                entries: MAX_SOURCE_SET_FOLDING_RANGE_ENTRIES,
                wire_bytes: 0,
            },
            FoldingRangeBudget {
                entries: 0,
                wire_bytes: MAX_SOURCE_SET_FOLDING_RANGE_WIRE_BYTES,
            },
        ] {
            let index = FoldingRangeIndex::from_occurrences(
                &source,
                vec![FoldingRangeOccurrence {
                    span: Span::new(fold_start, source.len() as u32),
                    kind: FOLDING_KIND_REGION,
                    text: FoldingRangeText::RegionLabel(Span::new(0, 4)),
                }],
                &mut exhausted,
            );
            assert!(index.entries.is_empty());
        }
    }

    #[test]
    fn folding_range_extraction_uses_utf16_columns_and_crlf_once() {
        let source = "fun choose(label: String = \"😀\") {\r\n\
                      \u{20}\u{20}if (true) {\r\n\
                      \u{20}\u{20}}\r\n\
                      }\r\n";
        let ranges = analyze_for_lsp(&[source])
            .remove(0)
            .folding_ranges
            .encode(source);

        assert_eq!(
            ranges,
            vec![
                json!({
                    "startLine": 0,
                    "startCharacter": 33,
                    "endLine": 3,
                    "endCharacter": 1,
                    "kind": "region",
                    "collapsedText": "{...}"
                }),
                json!({
                    "startLine": 1,
                    "startCharacter": 12,
                    "endLine": 2,
                    "endCharacter": 3,
                    "kind": "region",
                    "collapsedText": "{...}"
                })
            ]
        );
    }

    #[test]
    fn folding_range_extraction_matches_advanced_official_shapes() {
        let source = "package foldingadvanced\n\
                      \n\
                      //region Utilities\n\
                      // First line comment.\n\
                      // Second line comment.\n\
                      fun grouped(\n\
                      \u{20}\u{20}first: Int,\n\
                      \u{20}\u{20}second: Int,\n\
                      ): Int = listOf(\n\
                      \u{20}\u{20}first,\n\
                      \u{20}\u{20}second,\n\
                      ).map {\n\
                      \u{20}\u{20}it + 1\n\
                      }.sum()\n\
                      //endregion\n\
                      \n\
                      val raw = \"\"\"\n\
                      \u{20}\u{20}first\n\
                      \u{20}\u{20}second\n\
                      \"\"\".trimIndent()\n\
                      \n\
                      fun choose(value: Int): Int = when (value) {\n\
                      \u{20}\u{20}0 -> {\n\
                      \u{20}\u{20}\u{20}\u{20}1\n\
                      \u{20}\u{20}}\n\
                      \u{20}\u{20}else -> value\n\
                      }\n";
        let ranges = analyze_for_lsp(&[source])
            .remove(0)
            .folding_ranges
            .encode(source);

        assert_eq!(
            ranges,
            vec![
                json!({
                    "collapsedText": "Utilities",
                    "endCharacter": 11,
                    "endLine": 14,
                    "kind": "comment",
                    "startCharacter": 0,
                    "startLine": 2
                }),
                json!({
                    "collapsedText": "{...}",
                    "endCharacter": 7,
                    "endLine": 13,
                    "kind": "region",
                    "startCharacter": 11,
                    "startLine": 5
                }),
                json!({
                    "collapsedText": "(...)",
                    "endCharacter": 1,
                    "endLine": 11,
                    "kind": "region",
                    "startCharacter": 15,
                    "startLine": 8
                }),
                json!({
                    "collapsedText": "{...}",
                    "endCharacter": 1,
                    "endLine": 13,
                    "kind": "region",
                    "startCharacter": 6,
                    "startLine": 11
                }),
                json!({
                    "collapsedText": "\"\"\"first ...\"\"\"",
                    "endCharacter": 3,
                    "endLine": 19,
                    "kind": "region",
                    "startCharacter": 10,
                    "startLine": 16
                }),
                json!({
                    "collapsedText": "{...}",
                    "endCharacter": 1,
                    "endLine": 26,
                    "kind": "region",
                    "startCharacter": 30,
                    "startLine": 21
                }),
                json!({
                    "collapsedText": "{...}",
                    "endCharacter": 1,
                    "endLine": 26,
                    "kind": "region",
                    "startCharacter": 43,
                    "startLine": 21
                }),
                json!({
                    "collapsedText": "{...}",
                    "endCharacter": 3,
                    "endLine": 24,
                    "kind": "region",
                    "startCharacter": 7,
                    "startLine": 22
                })
            ]
        );
    }

    #[test]
    fn folding_range_extraction_matches_official_backticked_identifier_locations() {
        let source = "fun escaped(\n\
                      \u{20}\u{20}`)`: Int,\n\
                      ): Int = listOf(\n\
                      \u{20}\u{20}`)`,\n\
                      \u{20}\u{20}1,\n\
                      ).first()\n";
        let ranges = analyze_for_lsp(&[source])
            .remove(0)
            .folding_ranges
            .encode(source);

        assert_eq!(
            ranges,
            vec![
                json!({
                    "collapsedText": "{...}",
                    "endCharacter": 9,
                    "endLine": 5,
                    "kind": "region",
                    "startCharacter": 11,
                    "startLine": 0
                }),
                json!({
                    "collapsedText": "(...)",
                    "endCharacter": 1,
                    "endLine": 5,
                    "kind": "region",
                    "startCharacter": 15,
                    "startLine": 2
                })
            ]
        );
    }

    #[test]
    fn folding_range_extraction_matches_official_header_trivia_locations() {
        let source = "/* commented() */\n\
                      fun commented(/* ) commented() */\n\
                      \u{20}\u{20}value: String,\n\
                      ): String = listOf(\n\
                      \u{20}\u{20}value,\n\
                      \u{20}\u{20}\"ok\",\n\
                      ).first()\n\
                      \n\
                      fun rawHeader(value: String = \"\"\") rawHeader()\n\
                      \"\"\",\n\
                      ): String = listOf(\n\
                      \u{20}\u{20}value,\n\
                      \u{20}\u{20}\"ok\",\n\
                      ).first()\n";
        let ranges = analyze_for_lsp(&[source])
            .remove(0)
            .folding_ranges
            .encode(source);

        assert_eq!(
            ranges,
            vec![
                json!({
                    "collapsedText": "{...}",
                    "endCharacter": 9,
                    "endLine": 6,
                    "kind": "region",
                    "startCharacter": 13,
                    "startLine": 1
                }),
                json!({
                    "collapsedText": "(...)",
                    "endCharacter": 1,
                    "endLine": 6,
                    "kind": "region",
                    "startCharacter": 18,
                    "startLine": 3
                }),
                json!({
                    "collapsedText": "{...}",
                    "endCharacter": 9,
                    "endLine": 13,
                    "kind": "region",
                    "startCharacter": 13,
                    "startLine": 8
                }),
                json!({
                    "collapsedText": "\"\"\") rawHeader() ...\"\"\"",
                    "endCharacter": 3,
                    "endLine": 9,
                    "kind": "region",
                    "startCharacter": 30,
                    "startLine": 8
                }),
                json!({
                    "collapsedText": "(...)",
                    "endCharacter": 1,
                    "endLine": 13,
                    "kind": "region",
                    "startCharacter": 18,
                    "startLine": 10
                })
            ]
        );
    }

    #[test]
    fn signature_help_snapshot_is_compact_interned_and_queries_nested_calls() {
        assert_eq!(std::mem::size_of::<SignatureHelpCallEntry>(), 32);
        assert_eq!(std::mem::size_of::<SignatureHelpSignatureEntry>(), 12);
        assert_eq!(std::mem::size_of::<SignatureHelpParameterEntry>(), 12);
        assert_eq!(std::mem::size_of::<SignatureHelpArgumentEntry>(), 8);

        let source = "fun outer(value: Int, other: Int): Int = value + other\n\
                      fun inner(value: Int): Int = value\n\
                      fun use(): Int = outer(inner(1), 2) + outer(3, 4)\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols =
            SignatureHelpSymbols::from_source_set(&[source], &analysis.files, &analysis.symbols);
        let index = SignatureHelpIndex::from_file_analysis(
            source,
            &analysis.files[0],
            &symbols,
            &analysis.symbols,
            &mut SignatureHelpBudget::default(),
        );

        assert_eq!(index.entry_count(), 3);
        assert_eq!(
            index
                .strings
                .iter()
                .filter(|value| value.as_str() == "outer(value: Int, other: Int): Int")
                .count(),
            1
        );
        assert!(!index.strings.iter().any(|value| value == source));

        let inner = source.find("inner(1").unwrap() as u32 + "inner(".len() as u32;
        assert_eq!(
            index.encode(inner).unwrap()["signatures"][0]["label"],
            "inner(value: Int): Int"
        );
        let outer_second = source.find("inner(1), ").unwrap() as u32 + "inner(1), ".len() as u32;
        let outer = index.encode(outer_second).unwrap();
        assert_eq!(
            outer["signatures"][0]["label"],
            "outer(value: Int, other: Int): Int"
        );
        assert_eq!(outer["signatures"][0]["activeParameter"], 1);
    }

    #[test]
    fn signature_help_snapshot_respects_shared_call_and_wire_budgets() {
        let source = "fun answer(value: Int): Int = value\nfun use(): Int = answer(1)\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols =
            SignatureHelpSymbols::from_source_set(&[source], &analysis.files, &analysis.symbols);

        let mut call_budget = SignatureHelpBudget {
            calls: MAX_SOURCE_SET_SIGNATURE_HELP_CALLS,
            wire_bytes: 0,
        };
        let index = SignatureHelpIndex::from_file_analysis(
            source,
            &analysis.files[0],
            &symbols,
            &analysis.symbols,
            &mut call_budget,
        );
        assert_eq!(index.entry_count(), 0);

        let mut wire_budget = SignatureHelpBudget {
            calls: 0,
            wire_bytes: MAX_SOURCE_SET_SIGNATURE_HELP_WIRE_BYTES - 1,
        };
        let index = SignatureHelpIndex::from_file_analysis(
            source,
            &analysis.files[0],
            &symbols,
            &analysis.symbols,
            &mut wire_budget,
        );
        assert_eq!(index.entry_count(), 0);
        assert_eq!(wire_budget.calls, 0);

        let named_source = "fun answer(veryLongParameterName: Int): Int = veryLongParameterName\n\
             fun use(): Int = answer(veryLongParameterName = 1)\n";
        let named_analysis = analyze_standalone_source_set(&[named_source]);
        let named_symbols = SignatureHelpSymbols::from_source_set(
            &[named_source],
            &named_analysis.files,
            &named_analysis.symbols,
        );
        let mut name_budget = SignatureHelpBudget {
            calls: 0,
            // Enough for the 96-byte call and 16-byte argument record, but
            // deliberately not the retained name string.
            wire_bytes: MAX_SOURCE_SET_SIGNATURE_HELP_WIRE_BYTES - 112,
        };
        let index = SignatureHelpIndex::from_file_analysis(
            named_source,
            &named_analysis.files[0],
            &named_symbols,
            &named_analysis.symbols,
            &mut name_budget,
        );
        assert_eq!(index.entry_count(), 0);
        assert_eq!(name_budget.calls, 0);
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
    fn type_definition_snapshot_is_compact_source_free_and_exact() {
        assert_eq!(std::mem::size_of::<DefinitionEntry>(), 20);
        let target = "package typedef\n\
                      class TypeParityDerived\n\
                      class TypeParityHolder(val item: TypeParityDerived)\n\
                      val typeParityExplicitOrdinary: TypeParityDerived = TypeParityDerived()\n\
                      val typeParityInferredOrdinary = TypeParityDerived()\n";
        let use_source = "package typedef\n\
             fun use(input: TypeParityDerived): TypeParityDerived {\n\
             \u{20}\u{20}val inferred = input\n\
             \u{20}\u{20}val nullable: TypeParityDerived? = inferred\n\
             \u{20}\u{20}val copied = nullable\n\
             \u{20}\u{20}val holder = TypeParityHolder(inferred)\n\
             \u{20}\u{20}return holder.item\n\
             }\n";
        let analyses = analyze_for_lsp(&[target, use_source]);
        let index = &analyses[1].type_definitions;
        let derived_lo = target.find("TypeParityDerived").unwrap() as u32;
        let derived = DefinitionTarget {
            file: 0,
            span: Span::new(derived_lo, derived_lo + "TypeParityDerived".len() as u32),
        };
        let holder_lo = target.find("TypeParityHolder").unwrap() as u32;
        let holder = DefinitionTarget {
            file: 0,
            span: Span::new(holder_lo, holder_lo + "TypeParityHolder".len() as u32),
        };
        assert_eq!(
            analyses[0]
                .type_definitions
                .get(derived_lo)
                .collect::<Vec<_>>(),
            vec![derived]
        );
        for query in [
            target.find("typeParityExplicitOrdinary").unwrap(),
            target.find("typeParityInferredOrdinary").unwrap(),
        ] {
            assert_eq!(
                analyses[0]
                    .type_definitions
                    .get(query as u32)
                    .collect::<Vec<_>>(),
                vec![derived]
            );
        }

        for query in [
            use_source.find("input:").unwrap(),
            use_source.find("inferred =").unwrap(),
            use_source.find("= input").unwrap() + 2,
            use_source.find("nullable:").unwrap(),
            use_source.find("= nullable").unwrap() + 2,
            use_source.rfind("item").unwrap(),
        ] {
            assert_eq!(index.get(query as u32).collect::<Vec<_>>(), vec![derived]);
        }
        let constructor = use_source.find("TypeParityHolder(").unwrap() as u32;
        assert_eq!(index.get(constructor).collect::<Vec<_>>(), vec![holder]);
    }

    #[test]
    fn type_definition_covers_value_declaration_forms() {
        let source = "package typedforms\n\
                      open class TypeTarget\n\
                      data class TypeHolder(val value: TypeTarget)\n\
                      class TypeDelegate {\n\
                      \u{20}\u{20}operator fun getValue(thisRef: Any?, property: Any?): TypeTarget = TypeTarget()\n\
                      }\n\
                      fun declarations(input: TypeTarget) {\n\
                      \u{20}\u{20}lateinit var late: TypeTarget\n\
                      \u{20}\u{20}val delegated by TypeDelegate()\n\
                      \u{20}\u{20}val (destructured) = TypeHolder(input)\n\
                      \u{20}\u{20}for (element in arrayOf(input)) { element }\n\
                      \u{20}\u{20}val lambda = { lambdaInput: TypeTarget -> lambdaInput }\n\
                      }\n\
                      fun <T : TypeTarget> generic(input: T) {\n\
                      \u{20}\u{20}val explicitGeneric: T = input\n\
                      \u{20}\u{20}val copied = input\n\
                      }\n\
                      class TypeNamespace { class QualifiedTarget }\n\
                      fun <QualifiedTarget> qualified(input: TypeNamespace.QualifiedTarget) {\n\
                      \u{20}\u{20}val copiedQualified = input\n\
                      }\n\
                      open class SmartBase\n\
                      class SmartDerived : SmartBase()\n\
                      fun smart(value: SmartBase) {\n\
                      \u{20}\u{20}if (value is SmartDerived) { val narrowed = value }\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let target_lo = source.find("TypeTarget").unwrap() as u32;
        let target = DefinitionTarget {
            file: 0,
            span: Span::new(target_lo, target_lo + "TypeTarget".len() as u32),
        };
        for marker in [
            "late:",
            "delegated by",
            "destructured)",
            "element in",
            "lambdaInput:",
        ] {
            let query = source.find(marker).unwrap() as u32;
            assert_eq!(
                analysis.type_definitions.get(query).collect::<Vec<_>>(),
                vec![target],
                "{marker}"
            );
        }

        let explicit_generic = source.find("explicitGeneric:").unwrap() as u32;
        assert_eq!(analysis.type_definitions.get(explicit_generic).count(), 0);
        let copied = source.find("copied =").unwrap() as u32;
        assert_eq!(analysis.type_definitions.get(copied).count(), 0);
        let qualified_target_lo = source.find("class QualifiedTarget").unwrap() as u32 + 6;
        let copied_qualified = source.find("copiedQualified =").unwrap() as u32;
        assert_eq!(
            analysis
                .type_definitions
                .get(copied_qualified)
                .collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(
                    qualified_target_lo,
                    qualified_target_lo + "QualifiedTarget".len() as u32,
                ),
            }]
        );
        let smart_target_lo = source.find("class SmartDerived").unwrap() as u32 + 6;
        let narrowed = source.find("narrowed =").unwrap() as u32;
        assert_eq!(
            analysis.type_definitions.get(narrowed).collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(
                    smart_target_lo,
                    smart_target_lo + "SmartDerived".len() as u32,
                ),
            }]
        );
    }

    #[test]
    fn type_definition_snapshot_respects_the_shared_navigation_entry_budget() {
        let mut budget = NavigationBudget {
            entries: MAX_SOURCE_SET_NAVIGATION_ENTRIES - 1,
        };
        let occurrences = vec![
            DefinitionOccurrence {
                span: Span::new(0, 1),
                target: DefinitionTarget {
                    file: 0,
                    span: Span::new(10, 11),
                },
            },
            DefinitionOccurrence {
                span: Span::new(2, 3),
                target: DefinitionTarget {
                    file: 0,
                    span: Span::new(12, 13),
                },
            },
        ];
        let index = DefinitionIndex::from_occurrences(occurrences, &mut budget);

        assert_eq!(index.entry_count(), 1);
        assert_eq!(budget.entries, MAX_SOURCE_SET_NAVIGATION_ENTRIES);
    }

    #[test]
    fn definitions_in_later_files_take_priority_over_type_definitions() {
        let mut budgets = AnalysisBudgets::new();
        budgets.navigation.entries = MAX_SOURCE_SET_NAVIGATION_ENTRIES - 2;
        let occurrence = |file, lo| DefinitionOccurrence {
            span: Span::new(lo, lo + 1),
            target: DefinitionTarget {
                file,
                span: Span::new(10, 11),
            },
        };
        let mut first = DocumentAnalysis::empty();
        first.definitions =
            DefinitionIndex::from_occurrences(vec![occurrence(0, 0)], &mut budgets.navigation);
        let mut second = DocumentAnalysis::empty();
        second.definitions =
            DefinitionIndex::from_occurrences(vec![occurrence(1, 2)], &mut budgets.navigation);

        let analyses = finalize_type_definitions(
            vec![
                (first, vec![occurrence(0, 4)]),
                (second, vec![occurrence(1, 6)]),
            ],
            &mut budgets,
        );

        assert_eq!(analyses[0].definitions.entry_count(), 1);
        assert_eq!(analyses[1].definitions.entry_count(), 1);
        assert_eq!(analyses[0].type_definitions.entry_count(), 0);
        assert_eq!(analyses[1].type_definitions.entry_count(), 0);
    }

    #[test]
    fn definition_snapshot_reverse_query_reuses_the_same_compact_entries() {
        let source = "data class User(val name: String)\n\
                      fun greet(user: User): String = user.name\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let declaration = Span::new(20, 24);
        let targets = HashSet::from([DefinitionTarget {
            file: 0,
            span: declaration,
        }]);

        assert_eq!(
            analysis
                .definitions
                .occurrences_targeting(&targets)
                .map(|(span, _)| span)
                .collect::<Vec<_>>(),
            vec![
                declaration,
                Span::new(
                    source.rfind("name").unwrap() as u32,
                    source.rfind("name").unwrap() as u32 + 4,
                ),
            ]
        );
        assert_eq!(std::mem::size_of::<DefinitionEntry>(), 20);
    }

    #[test]
    fn definition_snapshot_reverse_query_scans_multiple_targets_together() {
        let target_a = DefinitionTarget {
            file: 0,
            span: Span::new(10, 11),
        };
        let target_b = DefinitionTarget {
            file: 1,
            span: Span::new(20, 21),
        };
        let mut budget = NavigationBudget::default();
        let index = DefinitionIndex::from_occurrences(
            vec![
                DefinitionOccurrence {
                    span: Span::new(0, 1),
                    target: target_a,
                },
                DefinitionOccurrence {
                    span: Span::new(2, 3),
                    target: target_b,
                },
                DefinitionOccurrence {
                    span: Span::new(4, 5),
                    target: DefinitionTarget {
                        file: 2,
                        span: Span::new(30, 31),
                    },
                },
            ],
            &mut budget,
        );
        let targets = HashSet::from([target_a, target_b]);

        assert_eq!(
            index.occurrences_targeting(&targets).collect::<Vec<_>>(),
            vec![(Span::new(0, 1), target_a), (Span::new(2, 3), target_b),]
        );
    }

    #[test]
    fn definition_snapshot_respects_the_source_set_entry_budget() {
        let mut budget = NavigationBudget {
            entries: MAX_SOURCE_SET_NAVIGATION_ENTRIES - 1,
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
        assert_eq!(budget.entries, MAX_SOURCE_SET_NAVIGATION_ENTRIES);
    }

    #[test]
    fn hover_index_returns_symbol_hover_with_identifier_span() {
        let source = "fun box(): Int { val answer = 40 + 2; return answer }";
        let index = analyze_for_lsp(&[source]).remove(0).hover;
        let offset = source.rfind("answer").unwrap() as u32 + 1;
        let hover = index.get(offset).expect("hover over local read");
        assert_eq!(hover.value, "val answer: Int");
        assert_eq!(
            &source[hover.span.lo as usize..hover.span.hi as usize],
            "answer"
        );
    }

    #[test]
    fn hover_index_deduplicates_values_into_twelve_byte_array_entries() {
        let index = analyze_for_lsp(&["fun box(answer: Int): Int = answer + answer"])
            .remove(0)
            .hover;
        assert!(index.entry_count() >= 3);
        assert!(index.value_count() <= 2);
        assert_eq!(std::mem::size_of::<HoverEntry>(), 12);
        let json = serde_json::to_value(&index).unwrap();
        assert_eq!(json["entries"][0].as_array().unwrap().len(), 3);
    }

    #[test]
    fn hover_index_respects_source_set_entry_and_wire_budgets() {
        let occurrences = vec![
            HoverOccurrence {
                span: Span::new(0, 1),
                value: "val first: Int".to_string(),
            },
            HoverOccurrence {
                span: Span::new(2, 3),
                value: "val second: Int".to_string(),
            },
        ];
        let mut entry_budget = HoverBudget {
            entries: MAX_SOURCE_SET_HOVER_ENTRIES - 1,
            wire_bytes: 0,
        };
        let index = HoverIndex::from_occurrences(occurrences, &mut entry_budget);
        assert_eq!(index.entry_count(), 1);
        assert_eq!(entry_budget.entries, MAX_SOURCE_SET_HOVER_ENTRIES);

        let mut wire_budget = HoverBudget {
            entries: 0,
            wire_bytes: MAX_SOURCE_SET_HOVER_WIRE_BYTES - 1,
        };
        let index = HoverIndex::from_occurrences(
            vec![HoverOccurrence {
                span: Span::new(0, 1),
                value: "val blocked: Int".to_string(),
            }],
            &mut wire_budget,
        );
        assert_eq!(index.entry_count(), 0);
        assert_eq!(wire_budget.entries, 0);
    }

    #[test]
    fn duplicate_hover_occurrences_do_not_consume_the_shared_budget() {
        let mut budget = HoverBudget::default();
        let index = HoverIndex::from_occurrences(
            vec![
                HoverOccurrence {
                    span: Span::new(0, 1),
                    value: "val answer: Int".to_string(),
                },
                HoverOccurrence {
                    span: Span::new(0, 1),
                    value: "val answer: Int".to_string(),
                },
            ],
            &mut budget,
        );

        assert_eq!(index.entry_count(), 1);
        assert_eq!(budget.entries, 1);
    }

    #[test]
    fn hover_omits_literal_expressions_like_the_official_server() {
        let source = "fun box(): String? = null";
        let index = analyze_for_lsp(&[source]).remove(0).hover;
        assert!(index.get(source.rfind("null").unwrap() as u32).is_none());
    }

    #[test]
    fn hover_covers_type_parameters_and_loop_variables() {
        let source = "fun <T> identity(value: T): T = value\n\
                      fun loop(): Int { for (item in 1..1) return item; return 0 }";
        let index = analyze_for_lsp(&[source]).remove(0).hover;

        let type_parameter = index
            .get((source.find("<T>").unwrap() + 1) as u32)
            .expect("type-parameter declaration hover");
        assert_eq!(type_parameter.value, "T");
        let loop_variable = index
            .get((source.find("item in").unwrap() + 1) as u32)
            .expect("loop-variable declaration hover");
        assert_eq!(loop_variable.value, "val item: Int");
    }

    #[test]
    fn hover_renders_inferred_nested_types_as_source_names() {
        let source = "class Outer { class Inner }\n\
                      fun use(): Int { val nested = Outer.Inner(); return nested.hashCode() }";
        let index = analyze_for_lsp(&[source]).remove(0).hover;
        let hover = index
            .get((source.rfind("nested").unwrap() + 1) as u32)
            .expect("inferred nested local hover");

        assert_eq!(hover.value, "val nested: Outer.Inner");
        assert!(!hover.value.contains('$'));
        assert!(!hover.value.contains('/'));
    }

    #[test]
    fn completion_renders_inferred_nested_returns_as_source_names() {
        let source = "class Outer { class Inner }\n\
                      fun nestedFactory() = Outer.Inner()\n\
                      fun use() = nestedF";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let candidate = index
            .complete(source, source.len() as u32)
            .into_iter()
            .find(|candidate| candidate.label == "nestedFactory")
            .expect("inferred-return completion");

        assert_eq!(candidate.label_description, Some("Outer.Inner"));
        assert!(!candidate.label_description.unwrap().contains('$'));
        assert!(!candidate.label_description.unwrap().contains('/'));
    }

    #[test]
    fn hover_uses_checked_destructuring_component_types() {
        let source = "data class Parts(val number: Int, val text: String)\n\
                      fun use(parts: Parts): Int { val (number, text) = parts; return number }";
        let index = analyze_for_lsp(&[source]).remove(0).hover;

        let number = index
            .get((source.rfind("number").unwrap() + 1) as u32)
            .expect("destructured number hover");
        assert_eq!(number.value, "val number: Int");
        let text = index
            .get((source.find("text) =").unwrap() + 1) as u32)
            .expect("destructured text hover");
        assert_eq!(text.value, "val text: String");
    }

    #[test]
    fn hover_detects_modifiers_across_declaration_lines() {
        let source = "class Meter(val value: Int)\n\
                      operator\nfun Meter.plus(other: Meter): Meter = this\n\
                      open class Parent { open val item: Int = 1 }\n\
                      class Child : Parent() {\noverride\nval item: Int = 2\n}";
        let index = analyze_for_lsp(&[source]).remove(0).hover;

        let function = index
            .get((source.find("plus").unwrap() + 1) as u32)
            .expect("operator function hover");
        assert!(
            function.value.starts_with("operator fun "),
            "{}",
            function.value
        );
        let property = index
            .get((source.rfind("item").unwrap() + 1) as u32)
            .expect("override property hover");
        assert!(
            property.value.starts_with("override val "),
            "{}",
            property.value
        );
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let candidates = index.complete(source, source.len() as u32);

        assert!(candidates.iter().any(|candidate| candidate.label == "name"
            && candidate.kind == CompletionKind::Variable as u8));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.label == "greeting"
                && candidate.kind == CompletionKind::Method as u8));
    }

    #[test]
    fn completion_snapshot_interns_strings_into_compact_array_entries() {
        let source = "fun demo(user: String) { val local: String = user; loc }";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let candidates = index.complete(source, source.len() as u32);

        assert!(candidates
            .iter()
            .any(|candidate| candidate.label == "inherited"
                && candidate.kind == CompletionKind::Variable as u8));
    }

    #[test]
    fn completion_does_not_offer_unimported_cross_package_symbols() {
        let sources = [
            "package hidden\nfun secret(): Int = 1",
            "package visible\nfun use(): Int = sec",
            "package consumer\nimport hidden.secret\nfun use(): Int = sec",
        ];
        let analysis = analyze_standalone_source_set(&sources);
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
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
        let class_symbols = CompletionSymbols::from_source_set(&class_analysis.files);
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
        let companion_symbols = CompletionSymbols::from_source_set(&companion_analysis.files);
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let offset = source.rfind('x').unwrap() as u32 + 1;
        let candidate = index
            .complete(source, offset)
            .into_iter()
            .find(|candidate| candidate.label == "x")
            .unwrap();

        assert_eq!(candidate.kind, 6);
        assert_eq!(candidate.label_detail, None);
        assert_eq!(candidate.label_description, Some("String"));
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
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
        let value_symbols = CompletionSymbols::from_source_set(&value_analysis.files);
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
        let parameter_symbols = CompletionSymbols::from_source_set(&parameter_analysis.files);
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
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);

        assert!(index
            .complete(source, source.len() as u32)
            .iter()
            .all(|candidate| candidate.label != "Inner"));
    }

    #[test]
    fn completion_budget_truncates_source_set_snapshots() {
        let source = "fun answer(): Int = 42";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
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
            HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
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
