//! Compact semantic data retained by interactive language-server queries.

use std::collections::{HashMap, HashSet};

#[cfg(test)]
use crate::compiler_analysis::analyze_standalone_source_set;
use crate::compiler_analysis::java;
use crate::compiler_analysis::{
    analyze_standalone_source_inputs, document_symbol_occurrences, folding_range_occurrences,
    hover_wire_cost, CompletionDetails, CompletionKind, CompletionSymbols, DefinitionOccurrence,
    DefinitionSymbols, DefinitionTarget, DocumentSymbolOccurrence, FileAnalysis,
    FoldingRangeOccurrence, FrontendSymbols, HighlightOccurrence, HighlightSymbols,
    HoverOccurrence, LibraryRef, SemanticLimits, SignatureCandidate, SignatureHelpCall,
    SignatureHelpSymbols, FOLDING_KIND_COMMENT, FOLDING_KIND_IMPORTS, FOLDING_KIND_REGION,
    MAX_LIBRARY_DEFINITION_BYTES, TEXT_BLOCK_COMMENT, TEXT_BRACES, TEXT_IMPORTS, TEXT_KDOC,
    TEXT_PARENTHESES, TEXT_RAW_STRING, TEXT_REGION_LABEL,
};
use krusty::diag::{Diagnostic, Span};
use krusty::source::{SourceInput, SourceKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// `(source lo, source hi, interned hover value id)`.
type HoverEntry = [u32; 3];

/// Compact semantic snapshot retained for hover queries after full compiler analysis is dropped.
#[derive(Clone, Default, Deserialize, Serialize)]
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
pub const MAX_SOURCE_SET_WORKSPACE_SYMBOL_ENTRIES: usize = 32 * 1024;
pub const MAX_WORKSPACE_SYMBOL_WIRE_BYTES: usize = 8 * 1024 * 1024;
const MAX_WORKSPACE_SYMBOL_QUERY_BYTES: usize = 1024;
const MAX_WORKSPACE_SYMBOL_PACKAGE_BYTES: usize = 1024 * 1024;
const MAX_WORKSPACE_SYMBOL_CONTAINER_DEPTH: usize = 128;
const MAX_SOURCE_SET_FOLDING_RANGE_ENTRIES: usize = 32 * 1024;
const MAX_SOURCE_SET_FOLDING_RANGE_WIRE_BYTES: usize = 8 * 1024 * 1024;
const FOLDING_RANGE_WIRE_FIXED_BYTES: usize = 192;
const MAX_SOURCE_SET_SIGNATURE_HELP_CALLS: usize = 32 * 1024;
const MAX_SOURCE_SET_SIGNATURE_HELP_WIRE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RETAINED_ANALYSIS_BYTES: usize = 64 * 1024 * 1024;

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
#[derive(Clone, Deserialize, Serialize)]
pub struct CompletionIndex {
    entries: Vec<CompletionEntry>,
    members: Vec<CompletionMemberEntry>,
    strings: Vec<String>,
    #[serde(default)]
    complete: bool,
}

impl Default for CompletionIndex {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            members: Vec::new(),
            strings: Vec::new(),
            complete: false,
        }
    }
}

pub struct Completion<'a> {
    pub label: &'a str,
    pub kind: u8,
    pub label_detail: Option<&'a str>,
    pub label_description: Option<&'a str>,
}

/// `(source lo, source hi, target file, target lo, target hi)`.
type DefinitionEntry = [u32; 5];

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct DefinitionIndex {
    entries: Vec<DefinitionEntry>,
}

/// Classpath definitions keyed by their source occurrence.
#[derive(Default, Clone, Deserialize, Serialize)]
pub struct LibraryDefinitionIndex {
    entries: Vec<[u32; 3]>,
    references: Vec<LibraryRef>,
}

impl LibraryDefinitionIndex {
    pub(crate) fn from_occurrences(
        mut occurrences: Vec<(Span, LibraryRef)>,
        budget: &mut NavigationBudget,
    ) -> Self {
        occurrences.sort_unstable_by(|left, right| {
            (left.0.lo, left.0.hi, &left.1).cmp(&(right.0.lo, right.0.hi, &right.1))
        });
        occurrences.dedup();
        occurrences.truncate(budget.remaining());

        let mut references = Vec::new();
        let mut reference_ids = HashMap::new();
        let mut entries = Vec::with_capacity(occurrences.len());
        for (span, reference) in occurrences {
            let id = match reference_ids.get(&reference) {
                Some(id) => *id,
                None => {
                    let Ok(id) = u32::try_from(references.len()) else {
                        break;
                    };
                    reference_ids.insert(reference.clone(), id);
                    references.push(reference);
                    id
                }
            };
            entries.push([span.lo, span.hi, id]);
        }
        budget.entries += entries.len();
        Self {
            entries,
            references,
        }
    }

    pub fn get(&self, offset: u32) -> Option<&LibraryRef> {
        let upper = self.entries.partition_point(|entry| entry[0] <= offset);
        let entry = upper
            .checked_sub(1)
            .and_then(|index| self.entries.get(index))
            .filter(|entry| offset < entry[1])?;
        self.references.get(entry[2] as usize)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

/// `(start line, start UTF-16 column, end line, end UTF-16 column,
/// kind + collapsed-text style, summary source byte lo, summary source byte hi)`.
type FoldingRangeEntry = [u32; 7];

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct FoldingRangeIndex {
    entries: Vec<FoldingRangeEntry>,
}

/// `(name id, range start line/character, range end line/character,
/// selection start line/character, selection end line/character, kind/deprecated/parent)`.
type DocumentSymbolEntry = [u32; 10];

/// Compact pre-positioned hierarchy retained after compiler analysis is dropped.
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct DocumentSymbolIndex {
    entries: Vec<DocumentSymbolEntry>,
    names: Vec<String>,
}

type WorkspaceSymbolEntry = [u32; 10];

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct WorkspaceSymbolIndex {
    entries: Vec<WorkspaceSymbolEntry>,
    packages: Vec<String>,
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
#[derive(Clone, Default, Deserialize, Serialize)]
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

impl WorkspaceSymbolIndex {
    pub(crate) fn from_source_set(sources: &[&str], files: &[FileAnalysis]) -> Self {
        let mut result = Self::default();
        let mut package_ids = HashMap::<String, u32>::new();
        let mut package_bytes = 0usize;

        'files: for (file_index, (source, analysis)) in sources.iter().zip(files).enumerate() {
            let package = analysis.file.package.as_deref().unwrap_or("");
            let Some(package_id) = intern_workspace_package(
                package,
                &mut result.packages,
                &mut package_ids,
                &mut package_bytes,
            ) else {
                break;
            };
            let occurrences = document_symbol_occurrences(
                source,
                analysis,
                MAX_SOURCE_SET_WORKSPACE_SYMBOL_ENTRIES.saturating_sub(result.entries.len()),
            );
            let mut retained = Vec::with_capacity(occurrences.len());
            let mut retained_indices = vec![None; occurrences.len()];
            for (occurrence_index, occurrence) in occurrences.iter().enumerate() {
                let Some(name) = source
                    .get(occurrence.selection.lo as usize..occurrence.selection.hi as usize)
                    .map(|name| name.trim_matches('`'))
                    .filter(|name| !name.is_empty())
                else {
                    continue;
                };
                if name != occurrence.name {
                    continue;
                }
                let parent = match occurrence.parent {
                    Some(parent) => {
                        let Some(parent) = retained_indices.get(parent).copied().flatten() else {
                            continue;
                        };
                        Some(parent)
                    }
                    None => None,
                };
                retained_indices[occurrence_index] = Some(retained.len());
                retained.push((occurrence, parent));
            }
            let positions = selected_positions(
                source,
                retained
                    .iter()
                    .flat_map(|(occurrence, _)| [occurrence.selection.lo, occurrence.selection.hi]),
            );
            let position = |offset| {
                let index = positions
                    .binary_search_by_key(&offset, |(offset, _)| *offset)
                    .expect("workspace-symbol offset must be positioned");
                positions[index].1
            };
            let entry_offset = result.entries.len();

            for (occurrence, parent) in retained {
                if result.entries.len() >= MAX_SOURCE_SET_WORKSPACE_SYMBOL_ENTRIES {
                    break 'files;
                }
                let start = position(occurrence.selection.lo);
                let end = position(occurrence.selection.hi);
                let parent = parent
                    .and_then(|parent| entry_offset.checked_add(parent))
                    .and_then(|parent| u32::try_from(parent).ok())
                    .and_then(|parent| parent.checked_add(1))
                    .unwrap_or(0);
                result.entries.push([
                    file_index as u32,
                    occurrence.selection.lo,
                    occurrence.selection.hi,
                    start[0],
                    start[1],
                    end[0],
                    end[1],
                    u32::from(occurrence.kind),
                    parent,
                    package_id,
                ]);
            }
        }
        result
    }

    pub fn remap_files(&mut self, remaps: &[(u32, u32)]) {
        for entry in &mut self.entries {
            if let Ok(index) = remaps.binary_search_by_key(&entry[0], |(candidate, _)| *candidate) {
                entry[0] = remaps[index].1;
            }
        }
    }

    pub fn merge_from(&mut self, other: Self) {
        let mut package_bytes = self.packages.iter().map(String::len).sum::<usize>();
        let mut package_ids = self
            .packages
            .iter()
            .enumerate()
            .map(|(index, value)| (value.clone(), index as u32))
            .collect::<HashMap<_, _>>();
        let mut identities = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (workspace_symbol_identity(entry), index as u32))
            .collect::<HashMap<_, _>>();
        let mut remapped_entries = Vec::with_capacity(other.entries.len());
        for mut entry in other.entries {
            if let Some(&index) = identities.get(&workspace_symbol_identity(&entry)) {
                remapped_entries.push(Some(index));
                continue;
            }
            if self.entries.len() >= MAX_SOURCE_SET_WORKSPACE_SYMBOL_ENTRIES {
                break;
            }
            let Some(package) = other.packages.get(entry[9] as usize) else {
                remapped_entries.push(None);
                continue;
            };
            let Some(package_id) = intern_workspace_package(
                package,
                &mut self.packages,
                &mut package_ids,
                &mut package_bytes,
            ) else {
                break;
            };
            entry[8] = entry[8]
                .checked_sub(1)
                .and_then(|parent| remapped_entries.get(parent as usize).copied().flatten())
                .and_then(|parent| parent.checked_add(1))
                .unwrap_or(0);
            entry[9] = package_id;
            let index = self.entries.len() as u32;
            identities.insert(workspace_symbol_identity(&entry), index);
            self.entries.push(entry);
            remapped_entries.push(Some(index));
        }
    }

    pub fn encode(&self, query: &str, source_set: &[(String, String)]) -> Vec<Value> {
        if query.len() > MAX_WORKSPACE_SYMBOL_QUERY_BYTES {
            return Vec::new();
        }
        let folded_query = query.to_lowercase();
        let mut result = Vec::new();
        let mut wire_bytes = 2usize;
        for (entry_index, entry) in self.entries.iter().enumerate() {
            let Some((uri, source)) = source_set.get(entry[0] as usize) else {
                continue;
            };
            let Some(name) = workspace_source_name(source, entry) else {
                continue;
            };
            if !name.to_lowercase().contains(&folded_query) {
                continue;
            }
            let container = self.container_name(entry_index, source_set);
            let symbol = json!({
                "name": name,
                "kind": entry[7],
                "containerName": container,
                "location": {
                    "uri": uri,
                    "range": {
                        "start": {"line": entry[3], "character": entry[4]},
                        "end": {"line": entry[5], "character": entry[6]},
                    },
                },
            });
            let symbol_bytes = serde_json::to_vec(&symbol).map_or(usize::MAX, |wire| wire.len());
            let next_bytes = wire_bytes.saturating_add(symbol_bytes).saturating_add(1);
            if next_bytes > MAX_WORKSPACE_SYMBOL_WIRE_BYTES {
                break;
            }
            wire_bytes = next_bytes;
            result.push(symbol);
        }
        result
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    fn container_name(&self, entry_index: usize, source_set: &[(String, String)]) -> String {
        let entry = self.entries[entry_index];
        let mut names = Vec::new();
        let mut parent = entry[8];
        while names.len() < MAX_WORKSPACE_SYMBOL_CONTAINER_DEPTH {
            let Some(parent_index) = parent.checked_sub(1).map(|parent| parent as usize) else {
                break;
            };
            let Some(parent_entry) = self.entries.get(parent_index) else {
                break;
            };
            let Some((_, source)) = source_set.get(parent_entry[0] as usize) else {
                break;
            };
            let Some(name) = workspace_source_name(source, parent_entry) else {
                break;
            };
            names.push(name);
            parent = parent_entry[8];
        }
        names.reverse();
        let package = self
            .packages
            .get(entry[9] as usize)
            .map(String::as_str)
            .unwrap_or("");
        if names.is_empty() {
            return package.to_string();
        }
        if package.is_empty() {
            return names.join(".");
        }
        format!("{package}.{}", names.join("."))
    }
}

fn workspace_symbol_identity(entry: &WorkspaceSymbolEntry) -> (DefinitionTarget, u32) {
    (
        DefinitionTarget {
            file: entry[0],
            span: Span::new(entry[1], entry[2]),
        },
        entry[7],
    )
}

fn workspace_source_name<'a>(source: &'a str, entry: &WorkspaceSymbolEntry) -> Option<&'a str> {
    source
        .get(entry[1] as usize..entry[2] as usize)
        .map(|name| name.trim_matches('`'))
        .filter(|name| !name.is_empty())
}

fn intern_workspace_package(
    value: &str,
    packages: &mut Vec<String>,
    ids: &mut HashMap<String, u32>,
    retained_bytes: &mut usize,
) -> Option<u32> {
    if let Some(&id) = ids.get(value) {
        return Some(id);
    }
    if value.len() > MAX_WORKSPACE_SYMBOL_PACKAGE_BYTES.saturating_sub(*retained_bytes) {
        return None;
    }
    let id = packages.len() as u32;
    let value = value.to_string();
    ids.insert(value.clone(), id);
    *retained_bytes += value.len();
    packages.push(value);
    Some(id)
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

    fn remap_files(&mut self, remaps: &[(u32, u32)]) {
        if remaps.is_empty() {
            return;
        }
        for entry in &mut self.entries {
            if let Ok(index) = remaps.binary_search_by_key(&entry[2], |(file, _)| *file) {
                entry[2] = remaps[index].1;
            }
        }
        self.entries.sort_unstable();
        self.entries.dedup();
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
        let mut truncated = false;
        let entries = scoped
            .into_iter()
            .filter_map(|symbol| {
                if !budget.reserve(
                    &symbol.label,
                    &symbol.details,
                    symbol.result_type.as_deref(),
                ) {
                    truncated = true;
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
                    truncated = true;
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
            complete: !truncated,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.complete
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
                .filter(|(_, entry)| entry[0] == receiver_type)
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
                (
                    !candidate.label.starts_with(context.prefix),
                    completion_kind_group(candidate.kind),
                    *index,
                )
            });
            let mut seen = HashSet::new();
            result.retain(|(_, candidate)| seen.insert(candidate.label));
            return result.into_iter().map(|(_, candidate)| candidate).collect();
        }

        let mut best_by_label = HashMap::<&str, (usize, u32, u32)>::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let label = self.strings[entry[3] as usize].as_str();
            if entry[0] > offset || offset > entry[1] || entry[2] > offset {
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
                !candidate.label.starts_with(context.prefix),
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
#[derive(Clone, Default, Deserialize, Serialize)]
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

pub struct MaterializedDefinition {
    pub path: std::path::PathBuf,
    pub text: String,
    pub lo: u32,
    pub hi: u32,
}

#[derive(Clone)]
pub struct DocumentAnalysis {
    pub diagnostics: Vec<Diagnostic>,
    pub hover: HoverIndex,
    pub completion: CompletionIndex,
    pub signature_help: SignatureHelpIndex,
    pub semantic_tokens: SemanticTokenIndex,
    pub definitions: DefinitionIndex,
    pub type_definitions: DefinitionIndex,
    pub implementations: DefinitionIndex,
    pub library_definitions: LibraryDefinitionIndex,
    pub document_symbols: DocumentSymbolIndex,
    pub workspace_symbols: WorkspaceSymbolIndex,
    pub folding_ranges: FoldingRangeIndex,
    pub implementation_relations: Vec<[u32; 6]>,
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
    library_definition_bytes: usize,
    pending_type_definitions: usize,
    pending_implementations: usize,
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
            library_definition_bytes: 0,
            pending_type_definitions: 0,
            pending_implementations: 0,
            document_symbol: DocumentSymbolBudget::default(),
            folding_range: FoldingRangeBudget::default(),
            signature_help: SignatureHelpBudget::default(),
        }
    }

    fn remaining_pending_navigation(&self) -> usize {
        self.navigation.remaining().min(
            MAX_SOURCE_SET_NAVIGATION_ENTRIES.saturating_sub(
                self.pending_type_definitions
                    .saturating_add(self.pending_implementations),
            ),
        )
    }

    fn retain_pending_navigation(
        &mut self,
        type_definitions: &mut Vec<DefinitionOccurrence>,
        implementations: &mut Vec<DefinitionOccurrence>,
    ) {
        let remaining = self.remaining_pending_navigation();
        type_definitions.truncate(remaining);
        implementations.truncate(remaining.saturating_sub(type_definitions.len()));
        self.pending_type_definitions += type_definitions.len();
        self.pending_implementations += implementations.len();
    }
}

impl DocumentAnalysis {
    pub(crate) fn from_file_analysis(
        source: &str,
        analysis: FileAnalysis,
        file_index: u32,
        indexes: &SourceSetIndexes<'_>,
        budgets: &mut AnalysisBudgets,
    ) -> (Self, Vec<DefinitionOccurrence>, Vec<DefinitionOccurrence>) {
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
        let pending_navigation_entries = budgets.remaining_pending_navigation();
        let mut semantic = analysis.semantic_occurrences(
            source,
            file_index,
            indexes.symbols,
            indexes.highlights,
            indexes.definitions,
            SemanticLimits {
                definition_entries: budgets.navigation.remaining(),
                type_definition_entries: pending_navigation_entries,
                implementation_entries: pending_navigation_entries,
                hover_entries: budgets.hover.remaining_entries(),
                hover_wire_bytes: budgets.hover.remaining_wire_bytes(),
                library_definition_wire_bytes: MAX_LIBRARY_DEFINITION_BYTES
                    .saturating_sub(budgets.library_definition_bytes),
            },
        );
        budgets.library_definition_bytes = budgets
            .library_definition_bytes
            .saturating_add(semantic.library_definition_bytes);
        budgets.retain_pending_navigation(
            &mut semantic.type_definitions,
            &mut semantic.implementations,
        );
        let hover = HoverIndex::from_occurrences(semantic.hovers, &mut budgets.hover);
        let semantic_tokens = SemanticTokenIndex::from_occurrences(source, semantic.highlights);
        let definitions =
            DefinitionIndex::from_occurrences(semantic.definitions, &mut budgets.navigation);
        let library_definitions = LibraryDefinitionIndex::from_occurrences(
            semantic.library_definitions,
            &mut budgets.navigation,
        );
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
                library_definitions,
                type_definitions: DefinitionIndex::default(),
                implementations: DefinitionIndex::default(),
                document_symbols,
                workspace_symbols: WorkspaceSymbolIndex::default(),
                folding_ranges,
                implementation_relations: Vec::new(),
            },
            semantic.type_definitions,
            semantic.implementations,
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
            implementations: DefinitionIndex::default(),
            library_definitions: LibraryDefinitionIndex::default(),
            document_symbols: DocumentSymbolIndex::default(),
            workspace_symbols: WorkspaceSymbolIndex::default(),
            folding_ranges: FoldingRangeIndex::default(),
            implementation_relations: Vec::new(),
        }
    }

    pub fn empty() -> Self {
        Self::with_diagnostics(Vec::new())
    }

    pub fn remap_navigation_files(&mut self, remaps: &[(u32, u32)]) {
        self.definitions.remap_files(remaps);
        self.type_definitions.remap_files(remaps);
        self.implementations.remap_files(remaps);
        self.workspace_symbols.remap_files(remaps);
        for relation in &mut self.implementation_relations {
            for file in [0, 3] {
                if let Ok(index) =
                    remaps.binary_search_by_key(&relation[file], |(candidate, _)| *candidate)
                {
                    relation[file] = remaps[index].1;
                }
            }
        }
        self.implementation_relations.sort_unstable();
        self.implementation_relations.dedup();
    }

    pub fn retained_wire_bytes(&self) -> usize {
        self.diagnostic_wire_bytes()
            .saturating_add(self.semantic_wire_bytes())
    }

    fn diagnostic_wire_bytes(&self) -> usize {
        self.diagnostics.iter().fold(0usize, |bytes, diagnostic| {
            bytes
                .saturating_add(96)
                .saturating_add(diagnostic.msg.len().saturating_mul(6))
        })
    }

    fn semantic_wire_bytes(&self) -> usize {
        serde_json::to_vec(&(
            &self.hover,
            &self.completion,
            &self.signature_help,
            &self.semantic_tokens,
            &self.definitions,
            &self.type_definitions,
            &self.implementations,
            &self.library_definitions,
            &self.document_symbols,
            &self.workspace_symbols,
            &self.folding_ranges,
            &self.implementation_relations,
        ))
        .map_or(usize::MAX, |wire| wire.len())
    }

    fn clear_semantic_indexes(&mut self) {
        self.hover = HoverIndex::default();
        self.completion = CompletionIndex::default();
        self.signature_help = SignatureHelpIndex::default();
        self.semantic_tokens = SemanticTokenIndex::default();
        self.definitions = DefinitionIndex::default();
        self.type_definitions = DefinitionIndex::default();
        self.implementations = DefinitionIndex::default();
        self.library_definitions = LibraryDefinitionIndex::default();
        self.document_symbols = DocumentSymbolIndex::default();
        self.workspace_symbols = WorkspaceSymbolIndex::default();
        self.folding_ranges = FoldingRangeIndex::default();
        self.implementation_relations.clear();
    }
}

pub fn merge_cross_document_implementations(analyses: &mut [DocumentAnalysis]) {
    merge_cross_document_implementations_with_limits(
        analyses,
        MAX_SOURCE_SET_NAVIGATION_ENTRIES,
        MAX_RETAINED_ANALYSIS_BYTES,
    );
}

fn merge_cross_document_implementations_with_limits(
    analyses: &mut [DocumentAnalysis],
    max_entries: usize,
    max_wire_bytes: usize,
) {
    const DEFINITION_ENTRY_MAX_WIRE_BYTES: usize = 64;

    let mut relation_pairs = HashSet::new();
    'relations: for analysis in analyses.iter_mut() {
        for relation in analysis.implementation_relations.drain(..) {
            if relation_pairs.len() >= max_entries {
                break 'relations;
            }
            relation_pairs.insert((
                DefinitionTarget {
                    file: relation[0],
                    span: Span::new(relation[1], relation[2]),
                },
                DefinitionTarget {
                    file: relation[3],
                    span: Span::new(relation[4], relation[5]),
                },
            ));
        }
    }
    for analysis in analyses.iter_mut() {
        analysis.implementation_relations.clear();
    }

    let mut relations = HashMap::<DefinitionTarget, Vec<DefinitionTarget>>::new();
    for (declaration, implementation) in relation_pairs {
        relations
            .entry(declaration)
            .or_default()
            .push(implementation);
    }
    for targets in relations.values_mut() {
        targets.sort_unstable_by_key(|target| (target.file, target.span.lo, target.span.hi));
        targets.dedup();
    }

    let retained_entries = analyses.iter().fold(0usize, |total, analysis| {
        total
            .saturating_add(analysis.definitions.entries.len())
            .saturating_add(analysis.type_definitions.entries.len())
            .saturating_add(analysis.implementations.entries.len())
    });
    let mut remaining_entries = max_entries.saturating_sub(retained_entries);
    let retained_wire_bytes = analyses.iter().fold(0usize, |total, analysis| {
        total.saturating_add(analysis.retained_wire_bytes())
    });
    let mut remaining_wire_bytes = max_wire_bytes.saturating_sub(retained_wire_bytes);
    for analysis in analyses {
        let mut additions = Vec::new();
        for definition in &analysis.definitions.entries {
            let target = DefinitionTarget {
                file: definition[2],
                span: Span::new(definition[3], definition[4]),
            };
            let Some(implementations) = relations.get(&target) else {
                continue;
            };
            for implementation in implementations {
                if remaining_entries == 0 || remaining_wire_bytes < DEFINITION_ENTRY_MAX_WIRE_BYTES
                {
                    break;
                }
                additions.push([
                    definition[0],
                    definition[1],
                    implementation.file,
                    implementation.span.lo,
                    implementation.span.hi,
                ]);
                remaining_entries -= 1;
                remaining_wire_bytes -= DEFINITION_ENTRY_MAX_WIRE_BYTES;
            }
            if remaining_entries == 0 || remaining_wire_bytes < DEFINITION_ENTRY_MAX_WIRE_BYTES {
                break;
            }
        }
        analysis.implementations.entries.extend(additions);
        analysis.implementations.entries.sort_unstable();
        analysis.implementations.entries.dedup();
    }
}

pub fn retain_analysis_wire_budget(analyses: &mut [DocumentAnalysis], max_bytes: usize) {
    let mut remaining = max_bytes;
    for analysis in analyses {
        let mut diagnostic_bytes = analysis.diagnostic_wire_bytes();
        while diagnostic_bytes > remaining && !analysis.diagnostics.is_empty() {
            let diagnostic = analysis.diagnostics.pop().unwrap();
            diagnostic_bytes = diagnostic_bytes
                .saturating_sub(96usize.saturating_add(diagnostic.msg.len().saturating_mul(6)));
        }
        remaining = remaining.saturating_sub(diagnostic_bytes);
        let semantic_bytes = analysis.semantic_wire_bytes();
        if semantic_bytes <= remaining {
            remaining -= semantic_bytes;
        } else {
            analysis.clear_semantic_indexes();
            remaining = remaining.saturating_sub(analysis.semantic_wire_bytes());
        }
    }
}

pub(crate) fn finalize_navigation(
    mut pending: Vec<(
        DocumentAnalysis,
        Vec<DefinitionOccurrence>,
        Vec<DefinitionOccurrence>,
    )>,
    budgets: &mut AnalysisBudgets,
) -> Vec<DocumentAnalysis> {
    for (analysis, occurrences, _) in &mut pending {
        analysis.type_definitions =
            DefinitionIndex::from_occurrences(std::mem::take(occurrences), &mut budgets.navigation);
    }
    for (analysis, _, occurrences) in &mut pending {
        analysis.implementations =
            DefinitionIndex::from_occurrences(std::mem::take(occurrences), &mut budgets.navigation);
    }
    pending
        .into_iter()
        .map(|(analysis, _, _)| analysis)
        .collect()
}

/// Analyze one source in an open source set and retain only data needed by editor queries.
pub fn analyze_for_lsp(sources: &[&str]) -> Vec<DocumentAnalysis> {
    analyze_for_lsp_with_navigation_limit(sources, MAX_SOURCE_SET_NAVIGATION_ENTRIES)
}

/// Register the classes each Java document declares, so Kotlin references to them navigate to the
/// Java source rather than to the classpath stub that made them resolvable.
pub(crate) fn register_java_declarations(
    definition_symbols: &mut DefinitionSymbols,
    sources: &[&str],
    java_documents: &[u32],
) {
    let mut declarations = Vec::new();
    let mut counts = HashMap::new();
    for &index in java_documents {
        let Some(source) = sources.get(index as usize) else {
            continue;
        };
        for declaration in java::global_declared_class_occurrences(source, index) {
            *counts.entry(declaration.0.clone()).or_insert(0usize) += 1;
            declarations.push(declaration);
        }
    }
    for (owner, target) in declarations {
        if counts.get(&owner) == Some(&1)
            && !definition_symbols.class_targets().contains_key(&owner)
        {
            definition_symbols.insert_class_target(owner, target);
        }
    }
}

pub(crate) fn apply_java_navigation(
    analyses: &mut [DocumentAnalysis],
    sources: &[&str],
    java_documents: &[u32],
    definition_symbols: &DefinitionSymbols,
    budgets: &mut AnalysisBudgets,
) {
    let limit = analyses.len().min(sources.len());
    let java_documents = java_documents
        .iter()
        .map(|index| *index as usize)
        .filter(|index| *index < limit)
        .collect::<Vec<_>>();
    if java_documents.is_empty() {
        return;
    }
    for &index in &java_documents {
        let mut targets = definition_symbols.class_targets().clone();
        targets.extend(java::declared_classes(sources[index], index as u32));
        let occurrences = java::definition_occurrences(sources[index], &targets);
        analyses[index].definitions =
            DefinitionIndex::from_occurrences(occurrences, &mut budgets.navigation);
    }
}

pub fn analyze_documents_for_lsp(documents: &[(&str, &str)]) -> Vec<DocumentAnalysis> {
    let inputs = documents
        .iter()
        .map(|(uri, source)| {
            if uri.ends_with(".java") {
                SourceInput::java(source)
            } else {
                SourceInput::kotlin(source)
            }
        })
        .collect::<Vec<_>>();
    analyze_source_inputs_for_lsp(&inputs, MAX_SOURCE_SET_NAVIGATION_ENTRIES)
}

fn analyze_for_lsp_with_navigation_limit(
    sources: &[&str],
    navigation_relation_limit: usize,
) -> Vec<DocumentAnalysis> {
    let inputs = sources
        .iter()
        .map(|source| SourceInput::kotlin(source))
        .collect::<Vec<_>>();
    analyze_source_inputs_for_lsp(&inputs, navigation_relation_limit)
}

fn analyze_source_inputs_for_lsp(
    inputs: &[SourceInput<'_>],
    navigation_relation_limit: usize,
) -> Vec<DocumentAnalysis> {
    let sources = inputs.iter().map(|input| input.text).collect::<Vec<_>>();
    let java_documents = inputs
        .iter()
        .enumerate()
        .filter(|(_, input)| input.kind == SourceKind::Java)
        .map(|(index, _)| index as u32)
        .collect::<Vec<_>>();
    let analysis = analyze_standalone_source_inputs(inputs);
    let highlight_symbols = HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
    let mut definition_symbols = DefinitionSymbols::from_source_set(
        &sources,
        &analysis.files,
        &analysis.symbols,
        navigation_relation_limit,
    );
    register_java_declarations(&mut definition_symbols, &sources, &java_documents);
    let completion_symbols = CompletionSymbols::from_source_set(&analysis.files);
    let signature_help_symbols =
        SignatureHelpSymbols::from_source_set(&sources, &analysis.files, &analysis.symbols);
    let workspace_symbols = WorkspaceSymbolIndex::from_source_set(&sources, &analysis.files);
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
        .zip(&sources)
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
    let mut analyses = finalize_navigation(pending, &mut budgets);
    apply_java_navigation(
        &mut analyses,
        &sources,
        &java_documents,
        &definition_symbols,
        &mut budgets,
    );
    if let Some(first) = analyses.first_mut() {
        first.workspace_symbols = workspace_symbols;
    }
    analyses
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler_analysis::CompletionKind;

    #[test]
    fn workspace_symbols_use_source_names_and_declaration_containers() {
        let source = "package workspaceparity\n\
                      class KrustyWorkspaceParityBox {\n\
                      \u{20}\u{20}fun nestedNeedle(): Int = 1\n\
                      }\n\
                      fun krustyWorkspaceParityNeedle(): Int = 2\n\
                      val krustyWorkspaceParityValue: Int = 3\n\
                      fun `when`(): Int = 4\n\
                      class Constructed(val value: Int)\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let index = WorkspaceSymbolIndex::from_source_set(&[source], &analysis.files);
        let source_set = vec![(
            "file:///WorkspaceSymbols.kt".to_string(),
            source.to_string(),
        )];

        assert_eq!(
            index.encode("KrustyWorkspaceParityBox", &source_set),
            vec![json!({
                "name": "KrustyWorkspaceParityBox",
                "kind": 5,
                "containerName": "workspaceparity",
                "location": {
                    "uri": "file:///WorkspaceSymbols.kt",
                    "range": {
                        "start": {"line": 1, "character": 6},
                        "end": {"line": 1, "character": 30},
                    },
                },
            })]
        );
        assert_eq!(
            index.encode("krustyworkspaceparitybox", &source_set).len(),
            1
        );
        assert!(index.encode("KWPB", &source_set).is_empty());
        assert_eq!(
            index.encode("nestedNeedle", &source_set),
            vec![json!({
                "name": "nestedNeedle",
                "kind": 6,
                "containerName": "workspaceparity.KrustyWorkspaceParityBox",
                "location": {
                    "uri": "file:///WorkspaceSymbols.kt",
                    "range": {
                        "start": {"line": 2, "character": 6},
                        "end": {"line": 2, "character": 18},
                    },
                },
            })]
        );
        assert_eq!(
            index
                .encode("krustyWorkspaceParityNeedle", &source_set)
                .len(),
            1
        );
        assert_eq!(
            index
                .encode("krustyWorkspaceParityValue", &source_set)
                .len(),
            1
        );
        assert_eq!(index.encode("when", &source_set)[0]["name"], "when");
        assert_eq!(index.encode("Constructed", &source_set).len(), 1);

        let default_source = "class DefaultPackageMarker\n";
        let default_analysis = analyze_standalone_source_set(&[default_source]);
        let default_index =
            WorkspaceSymbolIndex::from_source_set(&[default_source], &default_analysis.files);
        let encoded = default_index.encode(
            "DefaultPackageMarker",
            &[("file:///Default.kt".into(), default_source.into())],
        );
        assert_eq!(encoded[0]["containerName"], "");
    }

    #[test]
    fn workspace_symbol_merge_deduplicates_module_overlap_after_file_remapping() {
        let source = "package demo\nclass OverlapType\nfun sharedFunction() = 1\n";
        let first_analysis = analyze_standalone_source_set(&[source]);
        let second_analysis = analyze_standalone_source_set(&[source]);
        let mut first = WorkspaceSymbolIndex::from_source_set(&[source], &first_analysis.files);
        let mut second = WorkspaceSymbolIndex::from_source_set(&[source], &second_analysis.files);
        first.remap_files(&[(0, 3)]);
        second.remap_files(&[(0, 3)]);
        first.merge_from(second);
        let mut source_set = vec![
            ("file:///unused.kt".to_string(), String::new()),
            ("file:///unused2.kt".to_string(), String::new()),
            ("file:///unused3.kt".to_string(), String::new()),
            ("file:///Shared.kt".to_string(), source.to_string()),
        ];

        assert_eq!(first.encode("OverlapType", &source_set).len(), 1);
        assert_eq!(first.encode("sharedFunction", &source_set).len(), 1);
        source_set.truncate(3);
        assert!(first.encode("OverlapType", &source_set).is_empty());
    }

    #[test]
    fn workspace_symbol_snapshot_and_expanded_response_are_bounded() {
        let source = "Needle\n".repeat(MAX_SOURCE_SET_WORKSPACE_SYMBOL_ENTRIES);
        let entries = (0..MAX_SOURCE_SET_WORKSPACE_SYMBOL_ENTRIES)
            .map(|line| {
                let lo = (line * 7) as u32;
                [0, lo, lo + 6, line as u32, 0, line as u32, 6, 5, 0, 0]
            })
            .collect();
        let index = WorkspaceSymbolIndex {
            entries,
            packages: vec!["package".into()],
        };
        assert!(
            serde_json::to_vec(&index).unwrap().len() <= MAX_WORKSPACE_SYMBOL_WIRE_BYTES,
            "retained workspace-symbol index exceeded its wire budget"
        );
        let long_uri = format!("file:///{}.kt", "u".repeat(2048));
        let encoded = index.encode("needle", &[(long_uri, source)]);
        assert!(encoded.len() < MAX_SOURCE_SET_WORKSPACE_SYMBOL_ENTRIES);
        assert!(
            serde_json::to_vec(&encoded).unwrap().len() <= MAX_WORKSPACE_SYMBOL_WIRE_BYTES,
            "expanded workspace-symbol result exceeded its wire budget"
        );
    }

    #[test]
    fn duplicate_and_private_java_classes_are_not_global_targets() {
        let sources = [
            "package demo; class Same {}",
            "package demo; class Same {} class Outer { private class Secret {} }",
        ];
        let mut definitions = DefinitionSymbols::default();
        register_java_declarations(&mut definitions, &sources, &[0, 1]);
        assert!(!definitions.class_targets().contains_key("demo/Same"));
        assert!(!definitions
            .class_targets()
            .contains_key("demo/Outer$Secret"));
        assert!(definitions.class_targets().contains_key("demo/Outer"));
    }

    #[test]
    fn library_definition_index_round_trips_and_locates_by_offset() {
        let reference = LibraryRef {
            fqn: "kotlin/collections/CollectionsKt".to_string(),
            member_name: "listOf".to_string(),
            member_desc: "([Ljava/lang/Object;)Ljava/util/List;".to_string(),
        };
        let index = LibraryDefinitionIndex::from_occurrences(
            vec![
                (Span::new(10, 16), reference.clone()),
                (Span::new(30, 36), reference),
            ],
            &mut NavigationBudget::default(),
        );

        let json = serde_json::to_string(&index).unwrap();
        let restored: LibraryDefinitionIndex = serde_json::from_str(&json).unwrap();

        let hit = restored
            .get(13)
            .expect("offset inside the occurrence resolves");
        assert_eq!(hit.fqn, "kotlin/collections/CollectionsKt");
        assert_eq!(hit.member_name, "listOf");
        assert_eq!(restored.references.len(), 1);
        assert_eq!(restored.get(33).unwrap().member_name, "listOf");
        assert!(
            restored.get(20).is_none(),
            "offset outside the range misses"
        );
    }

    #[test]
    fn classpath_types_constructors_and_members_have_library_targets() {
        let classpath = krusty::toolchain::stdlib_classpath();
        if classpath.scan_types().is_empty() {
            return;
        }
        let source = concat!(
            "import kotlin.text.Regex\n",
            "fun use(input: Regex): Regex = Regex(input.pattern)\n",
        );
        let analysis = document_analysis_for(source);

        let type_offset = source.find("input: Regex").unwrap() as u32 + "input: ".len() as u32;
        let constructor_offset = source.rfind("Regex(").unwrap() as u32;
        let member_offset = source.find("pattern").unwrap() as u32;
        for offset in [type_offset, constructor_offset] {
            let reference = analysis
                .library_definitions
                .get(offset)
                .expect("classpath type target");
            assert_eq!(reference.fqn, "kotlin/text/Regex");
            assert!(reference.member_name.is_empty());
        }
        let member = analysis
            .library_definitions
            .get(member_offset)
            .expect("classpath member target");
        assert_eq!(member.fqn, "kotlin/text/Regex");
        assert!(!member.member_name.is_empty());
    }

    fn document_analysis_for(source: &str) -> DocumentAnalysis {
        let classpath = std::rc::Rc::new(krusty::toolchain::stdlib_classpath());
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let mut frontend = crate::compiler_analysis::analyze_source_set(&[source], platform);
        let highlights = HighlightSymbols::from_source_set(&frontend.files, &frontend.symbols);
        let definitions = DefinitionSymbols::from_source_set(
            &[source],
            &frontend.files,
            &frontend.symbols,
            MAX_SOURCE_SET_NAVIGATION_ENTRIES,
        );
        let completions = CompletionSymbols::from_source_set(&frontend.files);
        let signatures =
            SignatureHelpSymbols::from_source_set(&[source], &frontend.files, &frontend.symbols);
        let indexes = SourceSetIndexes::new(
            &frontend.symbols,
            &highlights,
            &definitions,
            &completions,
            &signatures,
        );
        let file = frontend.files.remove(0);
        DocumentAnalysis::from_file_analysis(source, file, 0, &indexes, &mut AnalysisBudgets::new())
            .0
    }

    #[test]
    fn a_parenthesized_base_class_navigates_to_its_source_declaration() {
        let source = concat!("open class Base\n", "class Derived : Base()\n");
        let analysis = document_analysis_for(source);

        let use_offset = source.rfind("Base").unwrap() as u32;
        let target = analysis
            .definitions
            .get(use_offset)
            .next()
            .expect("parenthesized base class resolves to its declaration");
        let declaration = source.find("Base").unwrap() as u32;
        assert_eq!(target.span.lo, declaration);
        assert_eq!(target.span.hi, declaration + "Base".len() as u32);
    }

    #[test]
    fn a_parenless_base_class_navigates_to_its_source_declaration() {
        let source = concat!(
            "open class Base\n",
            "class Derived : Base {\n",
            "    constructor() : super()\n",
            "}\n",
        );
        let analysis = document_analysis_for(source);

        let use_offset = source.rfind("Base").unwrap() as u32;
        let target = analysis
            .definitions
            .get(use_offset)
            .next()
            .expect("parenless base class resolves to its declaration");
        assert_eq!(target.span.lo, source.find("Base").unwrap() as u32);
    }

    #[test]
    fn a_classpath_base_class_has_a_library_target() {
        let classpath = krusty::toolchain::stdlib_classpath();
        if classpath.scan_types().is_empty() {
            return;
        }
        let source = concat!(
            "import kotlin.collections.AbstractMutableList\n",
            "class Numbers : AbstractMutableList<Int>()\n",
        );
        let analysis = document_analysis_for(source);

        let use_offset = source.rfind("AbstractMutableList").unwrap() as u32;
        let reference = analysis
            .library_definitions
            .get(use_offset)
            .expect("classpath base class target");
        assert_eq!(reference.fqn, "kotlin/collections/AbstractMutableList");
    }

    #[test]
    fn a_classpath_annotation_reference_has_a_library_target() {
        let classpath = krusty::toolchain::stdlib_classpath();
        if classpath.scan_types().is_empty() {
            return;
        }
        let source = "@Deprecated(\"old\")\nfun stale(): Int = 1\n";
        let analysis = document_analysis_for(source);

        let use_offset = source.find("Deprecated").unwrap() as u32;
        let reference = analysis
            .library_definitions
            .get(use_offset)
            .expect("classpath annotation target");
        assert_eq!(reference.fqn, "kotlin/Deprecated");
    }

    #[test]
    fn a_classpath_import_name_has_a_library_target() {
        let classpath = krusty::toolchain::stdlib_classpath();
        if classpath.scan_types().is_empty() {
            return;
        }
        let source = concat!(
            "import kotlin.text.Regex\n",
            "fun use(input: Regex): Int = input.pattern.length\n",
        );
        let analysis = document_analysis_for(source);

        let use_offset = source.find("Regex").unwrap() as u32;
        let reference = analysis
            .library_definitions
            .get(use_offset)
            .expect("imported classpath type target");
        assert_eq!(reference.fqn, "kotlin/text/Regex");
    }

    #[test]
    fn unrelated_identifiers_do_not_inherit_classpath_type_targets() {
        let classpath = krusty::toolchain::stdlib_classpath();
        if classpath.scan_types().is_empty() {
            return;
        }
        let source = concat!(
            "import unrelated.Regex\n",
            "fun consume(value: Int): Int = value\n",
            "fun use(): Int = consume(Regex = 1)\n",
        );
        let analysis = document_analysis_for(source);

        for offset in [
            source.find("Regex").unwrap() as u32,
            source.rfind("Regex").unwrap() as u32,
        ] {
            assert!(
                analysis.library_definitions.get(offset).is_none(),
                "an unrelated identifier must not resolve to kotlin.text.Regex"
            );
        }
    }

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
    fn navigation_file_remaps_canonicalize_duplicate_support_targets() {
        let occurrence = |file| DefinitionOccurrence {
            span: Span::new(0, 1),
            target: DefinitionTarget {
                file,
                span: Span::new(10, 11),
            },
        };
        let mut analysis = DocumentAnalysis::empty();
        analysis.definitions =
            DefinitionIndex::build(vec![occurrence(2), occurrence(7)], usize::MAX);
        analysis.type_definitions = DefinitionIndex::build(vec![occurrence(7)], usize::MAX);
        analysis.implementations = DefinitionIndex::build(vec![occurrence(7)], usize::MAX);

        analysis.remap_navigation_files(&[(7, 2)]);

        let expected = vec![DefinitionTarget {
            file: 2,
            span: Span::new(10, 11),
        }];
        assert_eq!(analysis.definitions.get(0).collect::<Vec<_>>(), expected);
        assert_eq!(
            analysis.type_definitions.get(0).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            analysis.implementations.get(0).collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn implementation_relations_merge_back_to_dependency_declarations() {
        let base = DefinitionTarget {
            file: 0,
            span: Span::new(4, 8),
        };
        let child = DefinitionTarget {
            file: 1,
            span: Span::new(10, 15),
        };
        let mut dependency = DocumentAnalysis::empty();
        dependency.definitions = DefinitionIndex::build(
            vec![DefinitionOccurrence {
                span: base.span,
                target: base,
            }],
            usize::MAX,
        );
        let mut consumer = DocumentAnalysis::empty();
        consumer.definitions = DefinitionIndex::build(
            vec![DefinitionOccurrence {
                span: Span::new(20, 24),
                target: base,
            }],
            usize::MAX,
        );
        consumer.implementation_relations = vec![[
            base.file,
            base.span.lo,
            base.span.hi,
            child.file,
            child.span.lo,
            child.span.hi,
        ]];
        let mut analyses = [dependency, consumer];

        merge_cross_document_implementations(&mut analyses);

        assert_eq!(
            analyses[0]
                .implementations
                .get(base.span.lo)
                .collect::<Vec<_>>(),
            [child]
        );
    }

    #[test]
    fn implementation_relation_expansion_respects_the_global_entry_budget() {
        let base = DefinitionTarget {
            file: 0,
            span: Span::new(4, 8),
        };
        let mut dependency = DocumentAnalysis::empty();
        dependency.definitions = DefinitionIndex::build(
            vec![DefinitionOccurrence {
                span: base.span,
                target: base,
            }],
            usize::MAX,
        );
        let mut consumer = DocumentAnalysis::empty();
        consumer.definitions = DefinitionIndex::build(
            vec![DefinitionOccurrence {
                span: Span::new(20, 24),
                target: base,
            }],
            usize::MAX,
        );
        consumer.implementation_relations = vec![
            [base.file, base.span.lo, base.span.hi, 1, 10, 15],
            [base.file, base.span.lo, base.span.hi, 2, 30, 35],
        ];
        let mut analyses = [dependency, consumer];

        merge_cross_document_implementations_with_limits(&mut analyses, 3, usize::MAX);

        assert_eq!(
            analyses
                .iter()
                .map(|analysis| analysis.implementations.entry_count())
                .sum::<usize>(),
            1
        );
        assert!(analyses
            .iter()
            .all(|analysis| analysis.implementation_relations.is_empty()));
    }

    #[test]
    fn retained_analysis_budget_drops_semantics_globally() {
        let target = DefinitionTarget {
            file: 0,
            span: Span::new(10, 11),
        };
        let mut analyses = [DocumentAnalysis::empty(), DocumentAnalysis::empty()];
        for analysis in &mut analyses {
            analysis.definitions = DefinitionIndex::build(
                vec![DefinitionOccurrence {
                    span: Span::new(0, 1),
                    target,
                }],
                usize::MAX,
            );
        }

        retain_analysis_wire_budget(&mut analyses, 1);

        assert!(analyses
            .iter()
            .all(|analysis| analysis.definitions.entry_count() == 0));
        assert!(analyses
            .iter()
            .all(|analysis| !analysis.completion.is_complete()));
    }

    #[test]
    fn retained_analysis_budget_debits_cleared_index_wire_bytes() {
        let target = DefinitionTarget {
            file: 0,
            span: Span::new(10, 11),
        };
        let occurrence = |lo| DefinitionOccurrence {
            span: Span::new(lo, lo + 1),
            target,
        };
        let mut first = DocumentAnalysis::empty();
        first.definitions = DefinitionIndex::build(vec![occurrence(0), occurrence(2)], usize::MAX);
        let mut second = DocumentAnalysis::empty();
        second.definitions = DefinitionIndex::build(vec![occurrence(0)], usize::MAX);
        let budget = second.retained_wire_bytes();
        let mut analyses = [first, second];

        retain_analysis_wire_budget(&mut analyses, budget);

        assert!(analyses
            .iter()
            .all(|analysis| analysis.definitions.entry_count() == 0));
    }

    #[test]
    fn retained_analysis_budget_counts_and_clears_library_definitions() {
        let mut analysis = DocumentAnalysis::empty();
        analysis.library_definitions = LibraryDefinitionIndex {
            entries: vec![[0, 1, 0]],
            references: vec![LibraryRef {
                fqn: "sample/".to_string() + &"X".repeat(1024),
                member_name: "value".into(),
                member_desc: "()I".into(),
            }],
        };
        let retained = analysis.retained_wire_bytes();
        let empty = DocumentAnalysis::empty().retained_wire_bytes();
        assert!(retained > empty);

        retain_analysis_wire_budget(std::slice::from_mut(&mut analysis), empty);

        assert!(analysis.library_definitions.is_empty());
        assert!(analysis.retained_wire_bytes() <= empty);
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

        let analyses = finalize_navigation(
            vec![
                (first, vec![occurrence(0, 4)], Vec::new()),
                (second, vec![occurrence(1, 6)], Vec::new()),
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
    fn transient_implementation_relations_are_deterministically_capped() {
        let source = "interface Root\n\
                      class A : Root\n\
                      class B : Root\n\
                      class C : Root\n";
        let analysis = analyze_for_lsp_with_navigation_limit(&[source], 1)
            .pop()
            .unwrap();
        let root = source.find("Root").unwrap() as u32;
        let a = source.find("A :").unwrap() as u32;

        assert_eq!(
            analysis.implementations.get(root).collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(a, a + 1),
            }]
        );
    }

    #[test]
    fn implementation_snapshot_is_compact_transitive_generic_and_overload_exact() {
        assert_eq!(std::mem::size_of::<DefinitionEntry>(), 20);
        let contract = "package impltest\n\
                        interface Contract<T> {\n\
                        \u{20}\u{20}fun convert(value: T): T\n\
                        \u{20}\u{20}fun pick(value: Int): String\n\
                        \u{20}\u{20}fun pick(value: String): String\n\
                        }\n";
        let implementations = "package impltest\n\
                               class Middle : Contract<String> {\n\
                               \u{20}\u{20}override fun convert(value: String): String = value\n\
                               \u{20}\u{20}override fun pick(value: Int): String = value.toString()\n\
                               \u{20}\u{20}override fun pick(value: String): String = value\n\
                               }\n\
                               class Leaf : Middle()\n";
        let use_source = "package impltest\n\
                          fun use(value: Contract<String>): String = value.convert(\"x\") + value.pick(1)\n";
        let analyses = analyze_for_lsp(&[contract, implementations, use_source]);
        let convert_target_lo = implementations.find("convert").unwrap() as u32;
        let convert_target = DefinitionTarget {
            file: 1,
            span: Span::new(
                convert_target_lo,
                convert_target_lo + "convert".len() as u32,
            ),
        };
        for (file, query) in [
            (0, contract.find("convert").unwrap()),
            (2, use_source.find("convert").unwrap()),
        ] {
            assert_eq!(
                analyses[file]
                    .implementations
                    .get(query as u32)
                    .collect::<Vec<_>>(),
                vec![convert_target]
            );
        }

        let int_pick_target_lo = implementations.find("pick").unwrap() as u32;
        let int_pick_target = DefinitionTarget {
            file: 1,
            span: Span::new(int_pick_target_lo, int_pick_target_lo + "pick".len() as u32),
        };
        assert_eq!(
            analyses[2]
                .implementations
                .get(use_source.rfind("pick").unwrap() as u32)
                .collect::<Vec<_>>(),
            vec![int_pick_target]
        );

        let contract_lo = contract.find("Contract").unwrap() as u32;
        let middle_lo = implementations.find("Middle").unwrap() as u32;
        let leaf_lo = implementations.find("Leaf").unwrap() as u32;
        assert_eq!(
            analyses[0]
                .implementations
                .get(contract_lo)
                .collect::<Vec<_>>(),
            vec![
                DefinitionTarget {
                    file: 1,
                    span: Span::new(middle_lo, middle_lo + "Middle".len() as u32),
                },
                DefinitionTarget {
                    file: 1,
                    span: Span::new(leaf_lo, leaf_lo + "Leaf".len() as u32),
                },
            ]
        );
    }

    #[test]
    fn generic_implementation_does_not_capture_an_unrelated_descendant_overload() {
        let source = "interface Parent<T> {\n\
                      \u{20}\u{20}fun route(value: T): String\n\
                      }\n\
                      open class Middle : Parent<String> {\n\
                      \u{20}\u{20}override fun route(value: String): String = value\n\
                      \u{20}\u{20}open fun route(value: Int): String = value.toString()\n\
                      }\n\
                      class Child : Middle() {\n\
                      \u{20}\u{20}override fun route(value: Int): String = value.toString()\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_route = source.find("fun route").unwrap() as u32 + "fun ".len() as u32;
        let middle_string_route =
            source.find("override fun route").unwrap() as u32 + "override fun ".len() as u32;
        let middle_int_route =
            source.find("open fun route").unwrap() as u32 + "open fun ".len() as u32;
        let child_int_route =
            source.rfind("override fun route").unwrap() as u32 + "override fun ".len() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_route)
                .collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(
                    middle_string_route,
                    middle_string_route + "route".len() as u32,
                ),
            }]
        );
        assert_eq!(
            analysis
                .implementations
                .get(middle_int_route)
                .collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(child_int_route, child_int_route + "route".len() as u32),
            }]
        );
    }

    #[test]
    fn generic_base_class_override_uses_source_span_substitution() {
        let source = "open class GenericBase<T> {\n\
                      \u{20}\u{20}open fun route(value: T): String = value.toString()\n\
                      }\n\
                      class GenericChild : GenericBase<String>() {\n\
                      \u{20}\u{20}override fun route(value: String): String = value\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_route = source.find("fun route").unwrap() as u32 + "fun ".len() as u32;
        let child_route = source.rfind("fun route").unwrap() as u32 + "fun ".len() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_route)
                .collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(child_route, child_route + "route".len() as u32),
            }]
        );
    }

    #[test]
    fn generic_base_arguments_ignore_same_named_constructor_defaults() {
        let source = "open class Parent<T> {\n\
                      \u{20}\u{20}open fun consume(value: T) {}\n\
                      }\n\
                      class Child(\n\
                      \u{20}\u{20}val parent: Parent<Int> = Parent<Int>(),\n\
                      ) : Parent<String>() {\n\
                      \u{20}\u{20}override fun consume(value: String) {}\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_consume = source.find("fun consume").unwrap() as u32 + "fun ".len() as u32;
        let child_consume = source.rfind("fun consume").unwrap() as u32 + "fun ".len() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_consume)
                .collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(child_consume, child_consume + "consume".len() as u32),
            }]
        );
    }

    #[test]
    fn generic_base_arguments_ignore_nested_same_named_constructor_arguments() {
        let source = "open class Parent<T>(ignored: Any? = null) {\n\
                      \u{20}\u{20}open fun consume(value: T) {}\n\
                      }\n\
                      class Child : Parent<String>(Parent<Int>()) {\n\
                      \u{20}\u{20}override fun consume(value: String) {}\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_consume = source.find("fun consume").unwrap() as u32 + "fun ".len() as u32;
        let child_consume = source.rfind("fun consume").unwrap() as u32 + "fun ".len() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_consume)
                .collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(child_consume, child_consume + "consume".len() as u32),
            }]
        );
    }

    #[test]
    fn method_type_parameter_shadows_the_class_type_parameter() {
        let source = "open class Parent<T> {\n\
                      \u{20}\u{20}open fun <T> route(value: T): T = value\n\
                      }\n\
                      class Child : Parent<String>() {\n\
                      \u{20}\u{20}override fun <U> route(value: U): U = value\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_route = source.find("route").unwrap() as u32;
        let child_route = source.rfind("route").unwrap() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_route)
                .collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(child_route, child_route + "route".len() as u32),
            }]
        );
    }

    #[test]
    fn method_type_parameter_does_not_match_a_concrete_child_parameter() {
        let source = "interface Parent {\n\
                      \u{20}\u{20}fun <T> route(value: T): T\n\
                      }\n\
                      class Child : Parent {\n\
                      \u{20}\u{20}override fun route(value: String): String = value\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_route = source.find("route").unwrap() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_route)
                .collect::<Vec<_>>(),
            Vec::<DefinitionTarget>::new()
        );
    }

    #[test]
    fn class_type_parameter_substitution_preserves_parameter_identity() {
        let source = "interface Parent<T> {\n\
                      \u{20}\u{20}fun route(value: T): String\n\
                      }\n\
                      class Child<A, B> : Parent<B> {\n\
                      \u{20}\u{20}override fun route(value: A): String = value.toString()\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_route = source.find("route").unwrap() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_route)
                .collect::<Vec<_>>(),
            Vec::<DefinitionTarget>::new()
        );
    }

    #[test]
    fn nested_class_type_parameter_substitution_matches_structurally() {
        let source = "open class Box<T>\n\
                      interface Parent<T> {\n\
                      \u{20}\u{20}fun route(value: T): String\n\
                      }\n\
                      class Child<A, B> : Parent<Box<B>> {\n\
                      \u{20}\u{20}override fun route(value: Box<B>): String = value.toString()\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_route = source.find("route").unwrap() as u32;
        let child_route = source.rfind("route").unwrap() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_route)
                .collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(child_route, child_route + "route".len() as u32),
            }]
        );
    }

    #[test]
    fn erased_reference_nullability_does_not_define_an_override() {
        let source = "open class Parent {\n\
                      \u{20}\u{20}open fun route(value: String?): String = value.orEmpty()\n\
                      }\n\
                      class Child : Parent() {\n\
                      \u{20}\u{20}override fun route(value: String): String = value\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_route = source.find("route").unwrap() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_route)
                .collect::<Vec<_>>(),
            Vec::<DefinitionTarget>::new()
        );
    }

    #[test]
    fn member_extension_receiver_is_part_of_the_override_signature() {
        let source = "interface Parent {\n\
                      \u{20}\u{20}fun String.route(): String\n\
                      }\n\
                      class Child : Parent {\n\
                      \u{20}\u{20}override fun Int.route(): String = toString()\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_route = source.find("route").unwrap() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_route)
                .collect::<Vec<_>>(),
            Vec::<DefinitionTarget>::new()
        );
    }

    #[test]
    fn override_search_crosses_a_non_declaring_intermediate_class() {
        let source = "interface Parent {\n\
                      \u{20}\u{20}fun route(): String\n\
                      }\n\
                      abstract class Middle : Parent\n\
                      class Child : Middle() {\n\
                      \u{20}\u{20}override fun route(): String = \"child\"\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_route = source.find("fun route").unwrap() as u32 + "fun ".len() as u32;
        let child_route = source.rfind("fun route").unwrap() as u32 + "fun ".len() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_route)
                .collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(child_route, child_route + "route".len() as u32),
            }]
        );
    }

    #[test]
    fn generic_override_substitution_crosses_an_intermediate_class() {
        let source = "interface Parent<T> {\n\
                      \u{20}\u{20}fun route(value: T): String\n\
                      }\n\
                      abstract class Middle<U> : Parent<U>\n\
                      class Child : Middle<String>() {\n\
                      \u{20}\u{20}override fun route(value: String): String = value\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let parent_route = source.find("fun route").unwrap() as u32 + "fun ".len() as u32;
        let child_route = source.rfind("fun route").unwrap() as u32 + "fun ".len() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(parent_route)
                .collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(child_route, child_route + "route".len() as u32),
            }]
        );
    }

    #[test]
    fn constructor_property_override_uses_the_exact_declaration_span() {
        let contract = "interface Named {\n  val title: String\n}\n";
        let implementation = "class NamedImpl(override val title: String) : Named\n";
        let use_source = "fun read(named: Named): String = named.title\n";
        let analyses = analyze_for_lsp(&[contract, implementation, use_source]);
        let target_lo = implementation.find("title").unwrap() as u32;
        let target = DefinitionTarget {
            file: 1,
            span: Span::new(target_lo, target_lo + "title".len() as u32),
        };

        for (file, query) in [
            (0, contract.find("title").unwrap()),
            (2, use_source.find("title").unwrap()),
        ] {
            assert_eq!(
                analyses[file]
                    .implementations
                    .get(query as u32)
                    .collect::<Vec<_>>(),
                vec![target]
            );
        }
    }

    #[test]
    fn backtick_annotation_does_not_make_constructor_override_final() {
        let source = "annotation class `final`\n\
                      open class Base(open val title: String)\n\
                      open class Child(@`final` override val title: String) : Base(title)\n\
                      class Leaf(override val title: String) : Child(title)\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let base_title = (source.find("open val title").unwrap() + "open val ".len()) as u32;
        let child_title =
            (source.find("override val title").unwrap() + "override val ".len()) as u32;
        let leaf_title =
            (source.rfind("override val title").unwrap() + "override val ".len()) as u32;

        assert_eq!(
            analysis
                .implementations
                .get(child_title)
                .collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(leaf_title, leaf_title + "title".len() as u32),
            }]
        );
        assert_eq!(
            analysis.implementations.get(base_title).collect::<Vec<_>>(),
            vec![
                DefinitionTarget {
                    file: 0,
                    span: Span::new(child_title, child_title + "title".len() as u32),
                },
                DefinitionTarget {
                    file: 0,
                    span: Span::new(leaf_title, leaf_title + "title".len() as u32),
                },
            ]
        );
    }

    #[test]
    fn private_constructor_property_is_not_implemented_by_a_same_named_child_property() {
        let source = "open class Base(private val title: String)\n\
                      class Child(val title: String) : Base(\"base\")\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let private_title = source.find("title").unwrap() as u32;

        assert_eq!(
            analysis
                .implementations
                .get(private_title)
                .collect::<Vec<_>>(),
            Vec::<DefinitionTarget>::new()
        );
    }

    #[test]
    fn pending_type_definitions_and_implementations_share_the_source_set_budget() {
        let mut budgets = AnalysisBudgets::new();
        budgets.pending_type_definitions = MAX_SOURCE_SET_NAVIGATION_ENTRIES - 4;
        let occurrence = |offset| DefinitionOccurrence {
            span: Span::new(offset, offset + 1),
            target: DefinitionTarget {
                file: 0,
                span: Span::new(offset + 10, offset + 11),
            },
        };
        let mut type_definitions = vec![occurrence(0), occurrence(1)];
        let mut implementations = vec![occurrence(2), occurrence(3), occurrence(4), occurrence(5)];

        budgets.retain_pending_navigation(&mut type_definitions, &mut implementations);

        assert_eq!(type_definitions.len(), 2);
        assert_eq!(implementations.len(), 2);
        assert_eq!(
            budgets.pending_type_definitions + budgets.pending_implementations,
            MAX_SOURCE_SET_NAVIGATION_ENTRIES
        );

        let mut later_type_definitions = vec![occurrence(6)];
        let mut later_implementations = vec![occurrence(7)];
        budgets.retain_pending_navigation(&mut later_type_definitions, &mut later_implementations);

        assert!(later_type_definitions.is_empty());
        assert!(later_implementations.is_empty());
    }

    #[test]
    fn definition_and_implementation_snapshots_share_the_navigation_budget() {
        let mut budget = NavigationBudget {
            entries: MAX_SOURCE_SET_NAVIGATION_ENTRIES - 2,
        };
        let definition = DefinitionIndex::from_occurrences(
            vec![DefinitionOccurrence {
                span: Span::new(0, 1),
                target: DefinitionTarget {
                    file: 0,
                    span: Span::new(10, 11),
                },
            }],
            &mut budget,
        );
        let implementation = DefinitionIndex::from_occurrences(
            vec![
                DefinitionOccurrence {
                    span: Span::new(2, 3),
                    target: DefinitionTarget {
                        file: 1,
                        span: Span::new(12, 13),
                    },
                },
                DefinitionOccurrence {
                    span: Span::new(4, 5),
                    target: DefinitionTarget {
                        file: 2,
                        span: Span::new(14, 15),
                    },
                },
            ],
            &mut budget,
        );

        assert_eq!(definition.entry_count(), 1);
        assert_eq!(implementation.entry_count(), 1);
        assert_eq!(budget.entries, MAX_SOURCE_SET_NAVIGATION_ENTRIES);
    }

    #[test]
    fn semantic_navigation_occurrences_share_the_construction_limit() {
        let source = "interface Root\nclass A : Root\nclass B : Root\n";
        let source_set = analyze_standalone_source_set(&[source]);
        let highlights = HighlightSymbols::from_source_set(&source_set.files, &source_set.symbols);
        let definitions = DefinitionSymbols::from_source_set(
            &[source],
            &source_set.files,
            &source_set.symbols,
            MAX_SOURCE_SET_NAVIGATION_ENTRIES,
        );
        let occurrences = source_set.files[0].semantic_occurrences(
            source,
            0,
            &source_set.symbols,
            &highlights,
            &definitions,
            SemanticLimits {
                definition_entries: 2,
                type_definition_entries: 0,
                implementation_entries: 2,
                hover_entries: 0,
                hover_wire_bytes: 0,
                library_definition_wire_bytes: 0,
            },
        );

        assert_eq!(occurrences.definitions.len(), 2);
        assert_eq!(
            occurrences.definitions.len() + occurrences.implementations.len(),
            2
        );
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
    fn zero_arity_nested_lambda_captures_outer_implicit_it_hover() {
        let source = "fun invoke(block: () -> Int): Int = block()\n\
                      fun capture(value: Int): Int {\n\
                          val outer: (Int) -> Int = { invoke { it } }\n\
                          return outer(value)\n\
                      }";
        let index = analyze_for_lsp(&[source]).remove(0).hover;
        let inner_it = source.rfind("it").unwrap() as u32 + 1;
        let hover = index
            .get(inner_it)
            .expect("captured outer implicit `it` hover");

        assert_eq!(hover.value, "it: Int");
    }

    #[test]
    fn zero_value_parameter_receiver_lambda_captures_outer_implicit_it_hover() {
        let source = "class Scope\n\
                      fun capture(value: Int): Int {\n\
                          val outer: (Int) -> Int = { with(Scope()) { it } }\n\
                          return outer(value)\n\
                      }";
        let index = analyze_for_lsp(&[source]).remove(0).hover;
        let inner_it = source.rfind("it").unwrap() as u32 + 1;
        let hover = index
            .get(inner_it)
            .expect("receiver lambda captures outer implicit `it`");

        assert_eq!(hover.value, "it: Int");
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
    fn completion_returns_prefix_independent_in_scope_candidates() {
        let source = "fun alphaOne(): Int = 1\nfun betaTwo(): Int = 2\nfun use(): Int = al";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let candidates = index.complete(source, source.len() as u32);

        assert!(candidates
            .iter()
            .any(|candidate| candidate.label == "alphaOne"));
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.label == "betaTwo"),
            "prefix-independent completion must include non-prefix-matching in-scope symbols"
        );
    }

    #[test]
    fn completion_ranks_prefix_matches_before_other_candidate_kinds() {
        let source =
            "fun alphaFunction(): Int = 1\nfun use(): Int { val betaVariable = 2; return al }";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let candidates = index.complete(source, source.len() as u32 - 2);
        let alpha = candidates
            .iter()
            .position(|candidate| candidate.label == "alphaFunction")
            .expect("matching function completion");
        let beta = candidates
            .iter()
            .position(|candidate| candidate.label == "betaVariable")
            .expect("non-matching variable completion");

        assert!(
            alpha < beta,
            "prefix matches must receive earlier completion ranks"
        );
    }

    #[test]
    fn completion_member_list_is_prefix_independent() {
        let source = concat!(
            "class Box(val alpha: Int, val beta: Int)\n",
            "fun use(box: Box) = box.al"
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);
        let candidates = index.complete(source, source.len() as u32);

        assert!(candidates
            .iter()
            .any(|candidate| candidate.label == "alpha"));
        assert!(
            candidates.iter().any(|candidate| candidate.label == "beta"),
            "prefix-independent member completion must include non-prefix-matching members"
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
        assert!(
            !index.is_complete(),
            "a budget-truncated snapshot must report itself incomplete"
        );
    }

    #[test]
    fn completion_wire_budget_truncates_source_set_snapshots() {
        let source = "fun answer(): Int = 42";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
        let mut budget = CompletionBudget {
            entries: 0,
            wire_bytes: MAX_SOURCE_SET_COMPLETION_WIRE_BYTES,
        };
        let index = CompletionIndex::from_file_analysis_with_budget(
            source,
            &analysis.files[0],
            &symbols,
            &mut budget,
        );

        assert_eq!(index.entry_count(), 0);
        assert!(!index.is_complete());
    }

    #[test]
    fn completion_reports_complete_for_an_untruncated_snapshot() {
        let source = "fun answer(): Int = 42";
        let analysis = analyze_standalone_source_set(&[source]);
        let symbols = CompletionSymbols::from_source_set(&analysis.files);
        let index = CompletionIndex::from_file_analysis(source, &analysis.files[0], &symbols);

        assert!(
            index.is_complete(),
            "an untruncated snapshot must report itself complete"
        );
    }

    #[test]
    fn absent_completion_snapshot_is_incomplete() {
        assert!(!CompletionIndex::default().is_complete());
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
    fn semantic_tokens_skip_control_flow_labels_and_mark_compound_assignment() {
        let source = concat!(
            "fun sumPositive(values: IntArray): Int {\n",
            "var result = 0\n",
            "outer@ for (value in values) {\n",
            "if (value < 0) continue@outer\n",
            "if (value == 0) break@outer\n",
            "result += value\n",
            "}\n",
            "return result\n",
            "}\n",
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let tokens = decoded_tokens(&index);

        assert!(
            !tokens
                .iter()
                .any(|&(line, start, length, _, _)| (line, start, length) == (3, 24, 5)),
            "control-flow labels are not semantic symbols: {tokens:?}"
        );
        assert!(
            !tokens
                .iter()
                .any(|&(line, start, length, _, _)| (line, start, length) == (4, 22, 5)),
            "control-flow labels are not semantic symbols: {tokens:?}"
        );
        assert!(
            tokens.contains(&(5, 7, 2, 21, 512)),
            "builtin compound assignment is an operator: {tokens:?}"
        );

        let return_source = "fun pick(n: Int): Int = n.let { if (it > 0) return@let it; return 0 }";
        let return_analysis = analyze_standalone_source_set(&[return_source]);
        let return_index = SemanticTokenIndex::from_file_analysis(
            return_source,
            &return_analysis.files[0],
            &return_analysis.symbols,
        );
        let return_tokens = decoded_tokens(&return_index);
        let label_start = return_source.find("@let").unwrap() as u32;
        let first_it = return_source.find("(it").unwrap() as u32 + 1;
        let returned_it = return_source.find("@let it").unwrap() as u32 + 5;
        assert!(
            return_tokens.contains(&(0, label_start, 4, 12, 0)),
            "labeled returns target a function at the full `@label` span: {return_tokens:?}"
        );
        assert!(
            return_tokens.contains(&(0, first_it, 2, 7, 4))
                && return_tokens.contains(&(0, returned_it, 2, 7, 4)),
            "implicit lambda it is a readonly parameter: {return_tokens:?}"
        );

        let receiver_source = concat!(
            "interface Base { fun value(): Int = 1 }\n",
            "class Outer : Base {\n",
            "override fun value(): Int = super<Base>@Outer.value()\n",
            "fun self(): Outer = noreturn@ this\n",
            "}\n",
        );
        let receiver_analysis = analyze_standalone_source_set(&[receiver_source]);
        let receiver_index = SemanticTokenIndex::from_file_analysis(
            receiver_source,
            &receiver_analysis.files[0],
            &receiver_analysis.symbols,
        );
        let receiver_tokens = decoded_tokens(&receiver_index);
        let receiver_line = receiver_source.lines().nth(2).unwrap();
        let super_start = receiver_line.find("super").unwrap() as u32;
        let base_start = receiver_line.find("<Base>").unwrap() as u32 + 1;
        let receiver_label = receiver_line.find("@Outer").unwrap() as u32;
        let this_start = receiver_source
            .lines()
            .nth(3)
            .unwrap()
            .find("this")
            .unwrap() as u32;
        assert!(
            receiver_tokens.contains(&(2, super_start, 5, 3, 32))
                && receiver_tokens.contains(&(2, base_start, 4, 3, 32))
                && receiver_tokens.contains(&(2, receiver_label, 6, 1, 0))
                && receiver_tokens.contains(&(3, this_start, 4, 1, 0)),
            "receiver labels use exact class/interface spans: {receiver_tokens:?}"
        );
    }

    #[test]
    fn semantic_tokens_mark_user_compound_assignment_as_user_operator() {
        let source = concat!(
            "class Accumulator {\n",
            "operator fun plusAssign(value: Int) {}\n",
            "}\n",
            "operator fun Accumulator.minusAssign(value: Int) {}\n",
            "fun use(acc: Accumulator) {\n",
            "acc += 1\n",
            "acc -= 1\n",
            "}\n",
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let tokens = decoded_tokens(&index);

        assert!(
            tokens.contains(&(5, 4, 2, 21, 0)),
            "member plusAssign is a user operator: {tokens:?}"
        );
        assert!(
            tokens.contains(&(6, 4, 2, 21, 8)),
            "extension minusAssign is a static user operator: {tokens:?}"
        );
    }

    #[test]
    fn semantic_tokens_match_official_unary_inc_and_constructor_sites() {
        let source = concat!(
            "class Counter {\n",
            "operator fun unaryMinus(): Counter = this\n",
            "operator fun inc(): Counter = this\n",
            "}\n",
            "class Marker\n",
            "operator fun Marker.not(): Marker = this\n",
            "fun use(input: Int, counter: Counter) {\n",
            "var current = counter\n",
            "val builtin = -input\n",
            "val user = -current\n",
            "current++\n",
            "val previous = current++\n",
            "val extension = !Marker()\n",
            "}\n",
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let tokens = decoded_tokens(&index);

        assert!(
            tokens.contains(&(1, 37, 4, 1, 0)),
            "member this: {tokens:?}"
        );
        assert!(
            tokens.contains(&(8, 14, 1, 21, 512)),
            "builtin unary operator: {tokens:?}"
        );
        assert!(
            tokens.contains(&(9, 11, 1, 21, 0)),
            "member unary operator: {tokens:?}"
        );
        assert!(
            tokens.contains(&(10, 7, 2, 21, 0)),
            "statement increment operator: {tokens:?}"
        );
        assert!(
            tokens.contains(&(11, 22, 2, 21, 0)),
            "expression increment operator: {tokens:?}"
        );
        assert!(
            tokens.contains(&(12, 16, 1, 21, 8)),
            "extension unary operator: {tokens:?}"
        );
        assert!(
            tokens.contains(&(12, 17, 6, 13, 0)),
            "constructor invocation: {tokens:?}"
        );
    }

    #[test]
    fn semantic_tokens_use_inc_target_for_member_and_index_storage() {
        let source = concat!(
            "class StorageCounter { operator fun inc(): StorageCounter = this }\n",
            "class StorageHolder(var value: StorageCounter)\n",
            "class StorageCounters(var value: StorageCounter) {\n",
            "operator fun get(index: Int): StorageCounter = value\n",
            "operator fun set(index: Int, value: StorageCounter) { this.value = value }\n",
            "}\n",
            "fun useStorage(holder: StorageHolder, counters: StorageCounters) {\n",
            "holder.value++\n",
            "counters[0]++\n",
            "}\n",
        );
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(krusty::toolchain::stdlib_classpath()),
        ));
        let analysis = crate::compiler_analysis::analyze_source_set(&[source], platform);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let tokens = decoded_tokens(&index);
        let types = analysis.files[0].types.as_ref().expect("checked types");
        let inc_calls = analysis.files[0]
            .file
            .expr_arena
            .iter()
            .enumerate()
            .filter_map(|(index, expression)| {
                let krusty::ast::Expr::Call { callee, args } = expression else {
                    return None;
                };
                matches!(
                    analysis.files[0].file.expr(*callee),
                    krusty::ast::Expr::Member { name, .. } if name == "inc" && args.is_empty()
                )
                .then_some(krusty::ast::ExprId(index as u32))
            })
            .collect::<Vec<_>>();

        assert!(
            tokens.contains(&(7, 12, 2, 21, 0)),
            "member-storage increment uses StorageCounter.inc; calls={:?}: {tokens:?}",
            inc_calls
                .iter()
                .map(|&call| (
                    call,
                    types.resolved_call_is_member(call),
                    types.resolved_call_is_extension(call),
                    types.resolved_call_owner(call)
                ))
                .collect::<Vec<_>>()
        );
        assert!(
            !tokens
                .iter()
                .any(|&(line, start, length, _, _)| (line, start, length) == (8, 11, 2)),
            "official Kotlin LSP omits the index-storage increment token: {tokens:?}"
        );
    }

    #[test]
    fn semantic_tokens_match_official_advanced_symbol_classification() {
        let source = concat!(
            "package tokenparity\n",
            "@Deprecated(\"old\") data class Record(val value: Int)\n",
            "interface Named { fun name(): String }\n",
            "object Registry { var current: Record? = null }\n",
            "enum class State { READY }\n",
            "typealias Alias = Record\n",
            "operator fun Record.plus(other: Record): Record = this\n",
            "fun use(input: Alias): Int {\n",
            "Registry.current = input\n",
            "return State.READY.ordinal + input.value\n",
            "}\n",
        );
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(krusty::toolchain::stdlib_classpath()),
        ));
        let analysis = crate::compiler_analysis::analyze_source_set(&[source], platform);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let tokens = decoded_tokens(&index);

        assert!(tokens.contains(&(1, 1, 10, 13, 0))); // annotation application: method
        assert!(tokens.contains(&(2, 10, 5, 3, 33))); // interface declaration: abstract
        assert!(tokens.contains(&(2, 22, 4, 13, 33))); // bodyless interface method: abstract
        assert!(tokens.contains(&(5, 10, 5, 4, 17))); // deprecated alias declaration
        assert!(tokens.contains(&(5, 18, 6, 4, 16))); // deprecated alias target
        assert!(tokens.contains(&(6, 13, 6, 4, 16))); // extension receiver type
        assert!(tokens.contains(&(7, 15, 5, 4, 16))); // alias use inherits target kind/modifiers
        assert!(!tokens
            .iter()
            .any(|&(line, start, _, _, _)| { line == 6 && start == 50 })); // `this` is not a semantic name token
        assert!(tokens.contains(&(9, 19, 7, 9, 516)), "tokens: {tokens:?}"); // inherited enum property
        assert!(tokens.contains(&(9, 27, 1, 21, 512))); // builtin operator
    }

    #[test]
    fn semantic_tokens_follow_type_alias_chains() {
        let source = concat!(
            "@Deprecated(\"old\") data class Record(val value: Int)\n",
            "typealias Alias = Record\n",
            "typealias Alias2 = Alias\n",
            "fun use(input: Alias2): Int = input.value\n",
        );
        let analysis = analyze_standalone_source_set(&[source]);
        let index =
            SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
        let tokens = decoded_tokens(&index);

        assert!(tokens.contains(&(2, 10, 6, 4, 17)));
        assert!(tokens.contains(&(2, 19, 5, 4, 16)));
        assert!(tokens.contains(&(3, 15, 6, 4, 16)));
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
        assert!(tokens.contains(&(9, lines[9].rfind("Old").unwrap() as u32, 3, 13, 0)));
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

    fn semantic_tokens_keep_type_alias_metadata_package_qualified() {
        let deprecated = "package old\n\
                          @Deprecated(\"old\") data class Record(val value: Int)\n\
                          typealias Alias = Record\n\
                          fun use(value: Alias): Alias = value\n";
        let plain = "package plain\n\
                     data class Record(val value: Int)\n\
                     typealias Alias = Record\n";
        let sources = [deprecated, plain];
        let analysis = analyze_standalone_source_set(&sources);
        let highlight_symbols =
            HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
        let index = SemanticTokenIndex::from_source_set_file_analysis(
            deprecated,
            &analysis.files[0],
            &analysis.symbols,
            &highlight_symbols,
        );
        let tokens = decoded_tokens(&index);

        assert!(tokens.contains(&(3, 15, 5, 4, 16)), "{tokens:?}");
        assert!(tokens.contains(&(3, 23, 5, 4, 16)), "{tokens:?}");
    }

    #[test]
    fn semantic_tokens_do_not_resolve_alias_cycles_through_bare_name_collisions() {
        let cycle = "package cycle\n\
                     typealias A = B\n\
                     typealias B = A\n\
                     fun use(value: A): A = value\n";
        let unrelated = "package other\n@Deprecated(\"old\") data class A(val value: Int)\n";
        let sources = [cycle, unrelated];
        let analysis = analyze_standalone_source_set(&sources);
        let highlight_symbols =
            HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
        let index = SemanticTokenIndex::from_source_set_file_analysis(
            cycle,
            &analysis.files[0],
            &analysis.symbols,
            &highlight_symbols,
        );
        let tokens = decoded_tokens(&index);

        for expected in [
            (1, 10, 1, 1, 1),
            (1, 14, 1, 1, 0),
            (2, 10, 1, 1, 1),
            (2, 14, 1, 1, 0),
            (3, 15, 1, 1, 0),
            (3, 19, 1, 1, 0),
        ] {
            assert!(tokens.contains(&expected), "{tokens:?}");
        }
    }

    #[test]
    fn semantic_tokens_do_not_escape_alias_cycles_through_default_imports() {
        let cycle = "package cycle\n\
                     typealias Trap = Other\n\
                     typealias Other = Trap\n\
                     fun use(value: Other): Other = value\n";
        let default_import =
            "package kotlin\n@Deprecated(\"old\") data class Trap(val value: Int)\n";
        let sources = [cycle, default_import];
        let analysis = analyze_standalone_source_set(&sources);
        let highlight_symbols =
            HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
        let index = SemanticTokenIndex::from_source_set_file_analysis(
            cycle,
            &analysis.files[0],
            &analysis.symbols,
            &highlight_symbols,
        );
        let tokens = decoded_tokens(&index);

        for expected in [
            (1, 10, 4, 1, 513),
            (1, 17, 5, 1, 512),
            (2, 10, 5, 1, 513),
            (2, 18, 4, 1, 512),
            (3, 15, 5, 1, 512),
            (3, 23, 5, 1, 512),
        ] {
            assert!(tokens.contains(&expected), "{tokens:?}");
        }
    }

    #[test]
    fn semantic_tokens_follow_imported_alias_chains() {
        let model = "package model\n\
                     @Deprecated(\"old\") data class Record(val value: Int)\n\
                     typealias Alias = Record\n";
        let bridge = "package bridge\n\
                      import model.Alias\n\
                      typealias Alias2 = Alias\n";
        let consumer = "package consumer\n\
                        import bridge.Alias2\n\
                        fun use(value: Alias2): Alias2 = value\n";
        let sources = [model, bridge, consumer];
        let analysis = analyze_standalone_source_set(&sources);
        let highlight_symbols =
            HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
        let index = SemanticTokenIndex::from_source_set_file_analysis(
            consumer,
            &analysis.files[2],
            &analysis.symbols,
            &highlight_symbols,
        );
        let tokens = decoded_tokens(&index);

        assert!(tokens.contains(&(2, 15, 6, 4, 16)), "{tokens:?}");
        assert!(tokens.contains(&(2, 24, 6, 4, 16)), "{tokens:?}");
    }

    #[test]
    fn semantic_tokens_preserve_ambiguous_alias_targets() {
        let deprecated = "package left\n@Deprecated(\"old\") data class Target(val value: Int)\n";
        let plain = "package right\ndata class Target(val value: Int)\n";
        let default_import = "package kotlin\n@Deprecated(\"old\") annotation class Target\n";
        let consumer = "package consumer\n\
                        import left.*\n\
                        import right.*\n\
                        typealias Alias = Target\n\
                        fun use(value: Alias): Alias = value\n";
        let sources = [deprecated, plain, default_import, consumer];
        let analysis = analyze_standalone_source_set(&sources);
        let highlight_symbols =
            HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
        let index = SemanticTokenIndex::from_source_set_file_analysis(
            consumer,
            &analysis.files[3],
            &analysis.symbols,
            &highlight_symbols,
        );
        let tokens = decoded_tokens(&index);

        for expected in [
            (3, 10, 5, 1, 513),
            (3, 18, 6, 1, 512),
            (4, 15, 5, 1, 512),
            (4, 23, 5, 1, 512),
        ] {
            assert!(tokens.contains(&expected), "{tokens:?}");
        }
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
