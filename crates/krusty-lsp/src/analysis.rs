//! Compact semantic data retained by interactive language-server queries.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

#[cfg(test)]
use crate::compiler_analysis::analyze_standalone_source_set;
use crate::compiler_analysis::java;
use crate::compiler_analysis::{
    analyze_standalone_source_inputs, document_symbol_occurrences, folding_range_occurrences,
    hover_wire_cost, parsed_file_symbols, CompletionDetails, CompletionKind, CompletionSymbols,
    DefinitionOccurrence, DefinitionSymbols, DefinitionTarget, DocumentSymbolOccurrence,
    FileAnalysis, FoldingRangeOccurrence, FrontendSymbols, HighlightOccurrence, HighlightSymbols,
    HoverOccurrence, LibraryRef, SemanticLimits, SignatureCandidate, SignatureHelpCall,
    SignatureHelpSymbols, FOLDING_KIND_COMMENT, FOLDING_KIND_IMPORTS, FOLDING_KIND_REGION,
    MAX_LIBRARY_DEFINITION_BYTES, TEXT_BLOCK_COMMENT, TEXT_BRACES, TEXT_IMPORTS, TEXT_KDOC,
    TEXT_PARENTHESES, TEXT_RAW_STRING, TEXT_REGION_LABEL,
};
use krusty::diag::{Diagnostic, Span};
use krusty::source::{SourceInput, SourceKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Default)]
struct JsonWireCounter {
    bytes: usize,
}

impl Write for JsonWireCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.checked_add(buffer.len()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "serialized JSON length overflow",
            )
        })?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn serialized_json_wire_bytes<T: ?Sized + Serialize>(
    value: &T,
) -> Result<usize, serde_json::Error> {
    let mut counter = JsonWireCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.bytes)
}

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
pub const MAX_WORKSPACE_SYMBOL_WIRE_BYTES: usize = 8 * 1024 * 1024;
/// Ceiling on symbols in one response, on top of the byte ceiling above.
///
/// A broad query over a project-wide index matches tens of thousands of declarations, and encoding
/// them is the dominant cost of answering -- for a client that keeps 100 (Zed's `MAX_MATCHES`) and
/// re-sorts what it keeps. Results are emitted strongest rung first, so the cap keeps the best of
/// them and, because encoding stops when it is reached, the weakest rungs are never even scanned.
pub const MAX_WORKSPACE_SYMBOL_RESPONSE_SYMBOLS: usize = 512;
const MAX_SOURCE_SET_WORKSPACE_SYMBOL_INDEX_WIRE_BYTES: usize = 8 * 1024 * 1024;
/// Ceiling on the retained project-wide index, which is a different thing from the source-set
/// budget above: that one bounds a single worker message, while this one bounds everything the
/// session knows about the workspace.
///
/// Sized from the reference corpora, measured by the `workspace_index_sizing_probe` test:
/// intellij-community (48,722 files, 277,173 declarations) accounts to 99.6 MiB under this
/// conservative accounting; the kotlin repo (698,516 declarations over 64,648 files) to ~117 MiB.
/// The requirement is a workspace holding at least two intellij-community-sized projects with
/// headroom left, so 512 MiB: ~3.2M declarations at the worst-case ~167 bytes/entry charge (real
/// retained bytes run about 3x smaller, ~52/entry). A pathological workspace stops growing the
/// index instead of the process.
pub(crate) const MAX_PROJECT_WORKSPACE_SYMBOL_INDEX_WIRE_BYTES: usize = 512 * 1024 * 1024;
pub(crate) const MAX_WORKSPACE_SYMBOL_QUERY_BYTES: usize = 1024;
const MAX_WORKSPACE_SYMBOL_CONTAINER_DEPTH: usize = 128;
/// A leading-wildcard query scans the retained index. Bound the greedy matcher's total transitions
/// as well as input and response bytes, so an adversarial `*aaaa...b` pattern cannot multiply a
/// maximum-sized name table into unbounded request latency.
pub(crate) const MAX_WORKSPACE_SYMBOL_GLOB_STEPS: usize = 32 * 1024 * 1024;
const JSON_U32_MAX_BYTES: usize = 10;
// Entry array and separator, plus one value in each search permutation.
const WORKSPACE_SYMBOL_ENTRY_MAX_WIRE_BYTES: usize =
    2 + 13 * JSON_U32_MAX_BYTES + 12 + 1 + 2 * (JSON_U32_MAX_BYTES + 1);
// Object keys, collection delimiters, and completeness.
const WORKSPACE_SYMBOL_INDEX_FIXED_WIRE_BYTES: usize = 256;
/// Per-file ceiling on parse-only workspace indexing. Not a format limit -- entry offsets are
/// `u32`, so 4 GiB is the hard ceiling -- but a guard for the engine thread, which parses sweep
/// chunks between serving interactive requests, and for memory: the file is read whole before
/// parsing, so a pathological sparse or generated source would otherwise stall requests and bloat
/// the process. The largest real-code file in the reference corpora is 334 KiB
/// (intellij-community) and the largest generated one 4.9 MiB (a k8s client), so 64 MiB only ever
/// skips pathological sources -- and a skip now names the file in the client log instead of
/// failing silently.
pub const MAX_INDEXED_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SOURCE_SET_FOLDING_RANGE_ENTRIES: usize = 32 * 1024;
const MAX_SOURCE_SET_FOLDING_RANGE_WIRE_BYTES: usize = 8 * 1024 * 1024;
const FOLDING_RANGE_WIRE_FIXED_BYTES: usize = 192;
const MAX_SOURCE_SET_SIGNATURE_HELP_CALLS: usize = 32 * 1024;
const MAX_SOURCE_SET_SIGNATURE_HELP_WIRE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RETAINED_ANALYSIS_BYTES: usize = 64 * 1024 * 1024;
// Response field names, delimiters, and array framing.
const ANALYSIS_RESPONSE_FIXED_WIRE_BYTES: usize = 512;

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

struct WorkspaceSymbolBudget {
    wire_bytes: usize,
    max_wire_bytes: usize,
}

impl WorkspaceSymbolBudget {
    fn new() -> Self {
        Self::with_limit(MAX_SOURCE_SET_WORKSPACE_SYMBOL_INDEX_WIRE_BYTES)
    }

    fn with_limit(max_wire_bytes: usize) -> Self {
        Self {
            wire_bytes: WORKSPACE_SYMBOL_INDEX_FIXED_WIRE_BYTES,
            max_wire_bytes,
        }
    }

    fn from_index_within(index: &WorkspaceSymbolIndex, max_wire_bytes: usize) -> Self {
        let mut budget = Self::with_limit(max_wire_bytes);
        let string_bytes = index
            .packages
            .iter()
            .chain(&index.names)
            .chain(&index.files)
            .fold(0usize, |bytes, value| {
                bytes.saturating_add(workspace_symbol_string_wire_cost(value))
            });
        budget.wire_bytes = budget
            .wire_bytes
            .saturating_add(
                index
                    .entries
                    .len()
                    .saturating_mul(WORKSPACE_SYMBOL_ENTRY_MAX_WIRE_BYTES),
            )
            .saturating_add(string_bytes);
        budget
    }

    fn remaining_entry_capacity(&self) -> usize {
        self.max_wire_bytes.saturating_sub(self.wire_bytes) / WORKSPACE_SYMBOL_ENTRY_MAX_WIRE_BYTES
    }

    fn reserve_merged_entry(
        &mut self,
        name: &str,
        new_name: bool,
        package: &str,
        new_package: bool,
    ) -> bool {
        self.reserve_merged_entry_in_file(name, new_name, package, new_package, "", false)
    }

    fn reserve_merged_entry_in_file(
        &mut self,
        name: &str,
        new_name: bool,
        package: &str,
        new_package: bool,
        file: &str,
        new_file: bool,
    ) -> bool {
        let bytes = WORKSPACE_SYMBOL_ENTRY_MAX_WIRE_BYTES
            .saturating_add(if new_name {
                workspace_symbol_string_wire_cost(name)
            } else {
                0
            })
            .saturating_add(if new_package {
                workspace_symbol_string_wire_cost(package)
            } else {
                0
            })
            .saturating_add(if new_file {
                workspace_symbol_string_wire_cost(file)
            } else {
                0
            });
        self.reserve(bytes)
    }

    fn reserve(&mut self, bytes: usize) -> bool {
        if bytes > self.max_wire_bytes.saturating_sub(self.wire_bytes) {
            return false;
        }
        self.wire_bytes += bytes;
        true
    }
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
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct CompletionIndex {
    entries: Vec<CompletionEntry>,
    members: Vec<CompletionMemberEntry>,
    strings: Vec<String>,
    #[serde(default)]
    complete: bool,
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

/// `(file, selection lo/hi, start line/character, end line/character, kind, parent + 1,
/// package id, name id, declaration lo/hi)`.
type WorkspaceSymbolEntry = [u32; 13];

/// How many oversized-file skips keep their URI for the client log; the rest stay a count.
pub(crate) const MAX_OMISSION_EXAMPLES: usize = 3;

/// Bounded provenance for an incomplete symbol index: which ceiling was hit and what it cost.
///
/// Carried beside `complete` because the flag alone cannot say *why* search is missing symbols,
/// and the client log built from this is the only place a user can learn which file to exclude or
/// which limit is undersized.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct WorkspaceIndexOmissions {
    /// Files skipped before parse for exceeding [`MAX_INDEXED_FILE_BYTES`].
    pub oversized_files: usize,
    /// Up to [`MAX_OMISSION_EXAMPLES`] `(uri, bytes)` witnesses of those skips.
    pub oversized_examples: Vec<(String, u64)>,
    /// Declarations dropped when a chunk or retention budget was spent.
    pub dropped_entries: usize,
    /// Chunk builds that stopped early on a spent budget, leaving later files unparsed.
    pub truncated_chunks: usize,
}

impl WorkspaceIndexOmissions {
    pub fn is_empty(&self) -> bool {
        self.oversized_files == 0 && self.dropped_entries == 0 && self.truncated_chunks == 0
    }

    fn note_oversized(&mut self, uri: &str, bytes: u64) {
        self.oversized_files += 1;
        if self.oversized_examples.len() < MAX_OMISSION_EXAMPLES {
            self.oversized_examples.push((uri.to_string(), bytes));
        }
    }

    fn absorb(&mut self, other: Self) {
        self.oversized_files += other.oversized_files;
        for example in other.oversized_examples {
            if self.oversized_examples.len() < MAX_OMISSION_EXAMPLES {
                self.oversized_examples.push(example);
            }
        }
        self.dropped_entries += other.dropped_entries;
        self.truncated_chunks += other.truncated_chunks;
    }
}

/// Bounded, searchable declarations retained from one assembled source set.
///
/// `entry[0]` addresses the index's own `files` table, so an entry describes a file whose text
/// nothing else retains. The builder runs where only source *positions* are known — the analysis
/// worker is handed sources, never URIs — so a freshly built index numbers entries by their
/// position in the analyzed source set and [`WorkspaceSymbolIndex::assign_uris`] converts those
/// positions into file ids at the one boundary that knows the mapping.
#[derive(Clone, Serialize)]
pub struct WorkspaceSymbolIndex {
    entries: Vec<WorkspaceSymbolEntry>,
    packages: Vec<String>,
    by_name: Vec<u32>,
    by_initials: Vec<u32>,
    names: Vec<String>,
    files: Vec<String>,
    complete: bool,
    /// Why `complete` is false, when it is: bounded, user-reportable provenance.
    omissions: WorkspaceIndexOmissions,
    /// Per-name-id match keys, derived from `names` and rebuilt with the search order. Held rather
    /// than recomputed because both fallback rungs test every name on every query: allocating a
    /// lowercase copy and a camel-hump expansion per entry is what made a project-wide index cost
    /// over a tenth of a second per keystroke on the thread that also serves edits.
    #[serde(skip)]
    lowercase_names: Vec<String>,
    #[serde(skip)]
    initials: Vec<String>,
}

impl Default for WorkspaceSymbolIndex {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            packages: Vec::new(),
            by_name: Vec::new(),
            by_initials: Vec::new(),
            names: Vec::new(),
            files: Vec::new(),
            complete: true,
            omissions: WorkspaceIndexOmissions::default(),
            lowercase_names: Vec::new(),
            initials: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
struct WorkspaceSymbolIndexWire {
    entries: Vec<WorkspaceSymbolEntry>,
    packages: Vec<String>,
    #[serde(default)]
    by_name: Vec<u32>,
    #[serde(default)]
    by_initials: Vec<u32>,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default = "workspace_symbol_index_complete")]
    complete: bool,
    #[serde(default)]
    omissions: WorkspaceIndexOmissions,
}

impl<'de> Deserialize<'de> for WorkspaceSymbolIndex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = WorkspaceSymbolIndexWire::deserialize(deserializer)?;
        let mut index = Self {
            entries: wire.entries,
            packages: wire.packages,
            by_name: wire.by_name,
            by_initials: wire.by_initials,
            names: wire.names,
            files: wire.files,
            complete: wire.complete,
            omissions: wire.omissions,
            lowercase_names: Vec::new(),
            initials: Vec::new(),
        };
        index.drop_invalid_entries();
        index.rebuild_search_order();
        Ok(index)
    }
}

fn workspace_symbol_index_complete() -> bool {
    true
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

/// The project-wide symbol layer, held as size-tiered segments.
///
/// One index would be simpler, but splicing a chunk into it costs a pass over everything already
/// there: the interning tables are rebuilt and both rank orders re-sorted. At the scale this exists
/// for -- 700k declarations over 64k files -- paying that per 128-file chunk is hundreds of full
/// re-sorts on the thread that also serves requests.
///
/// So a chunk becomes its own segment, and adjacent segments merge only while their sizes are
/// within a factor of two. Each declaration is therefore rewritten a logarithmic number of times
/// rather than once per later chunk, and the segment count stays around `log2(entries / chunk)` --
/// under a dozen for either reference corpus. Segments are disjoint by file, so a query reads them
/// in sequence with no reconciliation beyond the shadowing every layer already does.
pub struct ProjectSymbolIndex {
    /// Oldest and largest first; the newest chunk is last.
    segments: Vec<WorkspaceSymbolIndex>,
    /// Tracked here rather than read back off the segments, because a chunk whose every file was
    /// skipped contributes no segment to carry the flag.
    complete: bool,
    /// Aggregated from every admitted chunk, and held here rather than on the segments: a chunk
    /// trimmed down to zero entries contributes no segment, and a merge would otherwise re-count
    /// what its operands recorded.
    omissions: WorkspaceIndexOmissions,
}

impl Default for ProjectSymbolIndex {
    fn default() -> Self {
        Self {
            segments: Vec::new(),
            complete: true,
            omissions: WorkspaceIndexOmissions::default(),
        }
    }
}

impl ProjectSymbolIndex {
    /// Re-index `uris` from `segment`, dropping whatever any segment held for them.
    pub fn replace_files(&mut self, uris: &[String], segment: WorkspaceSymbolIndex) {
        self.replace_files_within(uris, segment, MAX_PROJECT_WORKSPACE_SYMBOL_INDEX_WIRE_BYTES);
    }

    /// The budgeted implementation behind [`Self::replace_files`]. Keeping the ceiling here, at
    /// the aggregate that owns every segment, prevents an unmerged tail from sitting outside a
    /// limit that was previously checked only when two adjacent segments happened to coalesce.
    fn replace_files_within(
        &mut self,
        uris: &[String],
        mut segment: WorkspaceSymbolIndex,
        max_wire_bytes: usize,
    ) {
        for existing in &mut self.segments {
            existing.remove_files(uris);
        }
        self.segments.retain(|segment| segment.entry_count() > 0);

        // Spend every untouched segment before admitting the replacement. The accounting is the
        // same conservative entry/string accounting used by merge budgets, so adding a small tail
        // cannot exceed the whole-layer ceiling merely because its neighbour is more than twice
        // its size and therefore does not merge yet.
        let reserved = self
            .segments
            .iter()
            .map(WorkspaceSymbolIndex::retained_wire_bytes)
            .sum::<usize>();
        segment.retain_accounted_wire_budget(max_wire_bytes.saturating_sub(reserved));
        self.complete &= segment.is_complete();
        // Drained into the aggregate BEFORE the zero-entry return: a chunk trimmed to nothing is
        // precisely the omission most worth reporting.
        self.omissions.absorb(segment.take_omissions());
        if segment.entry_count() == 0 {
            return;
        }
        self.segments.push(segment);
        while self.segments.len() >= 2 {
            let last = self.segments[self.segments.len() - 1].entry_count();
            let previous = self.segments[self.segments.len() - 2].entry_count();
            if previous > last.saturating_mul(2) {
                break;
            }
            let merged = self
                .segments
                .pop()
                .expect("two segments were just observed");
            // The ceiling is on the whole layer, so what the untouched segments already hold is
            // spent before this merge starts -- their string tables as well as their entries, or
            // the layer settles above its ceiling by whatever those tables weigh.
            let reserved = self.segments[..self.segments.len() - 1]
                .iter()
                .map(WorkspaceSymbolIndex::retained_wire_bytes)
                .sum::<usize>();
            let target = self
                .segments
                .last_mut()
                .expect("two segments were just observed");
            target.merge_within(merged, max_wire_bytes.saturating_sub(reserved));
            // Both operands were drained at their own admission, so anything on the target now is
            // merge-drop provenance; left there it would never be read again and the client log
            // would fall back to the vague no-cause clause.
            let residue = target.take_omissions();
            self.omissions.absorb(residue);
        }
    }

    /// Newest first, which is the order a query must shadow them in.
    pub fn layers(&self) -> Vec<&WorkspaceSymbolIndex> {
        self.segments.iter().rev().collect()
    }

    pub fn entry_count(&self) -> usize {
        self.segments
            .iter()
            .map(WorkspaceSymbolIndex::entry_count)
            .sum()
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Whether every declaration the sweep offered is actually in here. False once a file was too
    /// large to parse or the layer reached its retention ceiling.
    pub fn is_complete(&self) -> bool {
        self.complete && self.segments.iter().all(WorkspaceSymbolIndex::is_complete)
    }

    /// What is known to be missing and why, aggregated across every chunk this index absorbed.
    pub fn omissions(&self) -> &WorkspaceIndexOmissions {
        &self.omissions
    }
}

/// The match qualities the ladder walks, strongest first.
#[derive(Clone, Copy)]
pub(crate) enum WorkspaceSymbolRung {
    /// An empty query asks for everything.
    Every,
    Glob,
    NamePrefix,
    InitialsPrefix,
    InitialsSubsequence,
    NameSubsequence,
}

/// One query's shape plus the files a higher layer already answers for.
struct EncodeScope<'a> {
    query: &'a WorkspaceQuery,
    suppressed: &'a HashSet<u32>,
}

/// Interning tables shared across the files one index build visits.
#[derive(Default)]
struct WorkspaceSymbolInterning {
    packages: HashMap<String, u32>,
    names: HashMap<String, u32>,
    files: HashMap<String, u32>,
}

impl WorkspaceSymbolIndex {
    pub(crate) fn from_source_set(sources: &[&str], files: &[FileAnalysis]) -> Self {
        let mut result = Self::default();
        let mut budget = WorkspaceSymbolBudget::new();
        let mut interning = WorkspaceSymbolInterning::default();

        for (file_index, (source, analysis)) in sources.iter().zip(files).enumerate() {
            let capacity = budget.remaining_entry_capacity();
            let occurrences =
                document_symbol_occurrences(source, &analysis.file, capacity.saturating_add(1));
            if !result.push_file(
                file_index as u32,
                source,
                analysis.file.package.as_deref().unwrap_or(""),
                occurrences,
                &mut budget,
                &mut interning,
            ) {
                break;
            }
        }
        result.rebuild_search_order();
        result
    }

    /// Index files nobody has opened, from their text on disk.
    ///
    /// Parse-only: nothing here needs resolution, and full analysis of a whole project would cost
    /// orders of magnitude more than the search it serves. A file past
    /// [`MAX_INDEXED_FILE_BYTES`] is skipped rather than parsed -- a single generated or sparse
    /// multi-megabyte source otherwise stalls the build for tens of seconds -- and marks the index
    /// incomplete so callers can report the gap.
    pub fn from_disk_sources(files: &[(&str, &str)]) -> Self {
        Self::from_uri_sources(files.iter().copied())
    }

    /// Build from any URI/source iterator, consuming and dropping each source after its parse.
    ///
    /// The disk adapter yields owned `String`s lazily, while tests usually borrow string slices.
    /// Keeping both origins on this one ingestion path prevents the file reader from retaining a
    /// whole 128-file chunk (up to roughly 512 MiB at the per-file ceiling) before parsing begins.
    pub(crate) fn from_uri_sources<I, U, S>(files: I) -> Self
    where
        I: IntoIterator<Item = (U, S)>,
        U: AsRef<str>,
        S: AsRef<str>,
    {
        let mut result = Self::default();
        let mut budget = WorkspaceSymbolBudget::new();
        let mut interning = WorkspaceSymbolInterning::default();

        for (uri, source) in files {
            let uri = uri.as_ref();
            let source = source.as_ref();
            if source.len() > MAX_INDEXED_FILE_BYTES {
                result.note_oversized_file(uri, source.len() as u64);
                continue;
            }
            if !interning.files.contains_key(uri)
                && !budget.reserve(workspace_symbol_string_wire_cost(uri))
            {
                result.complete = false;
                result.omissions.truncated_chunks += 1;
                break;
            }
            let capacity = budget.remaining_entry_capacity();
            let parsed = parsed_file_symbols(source, capacity.saturating_add(1));
            let file = intern_workspace_string(uri, &mut result.files, &mut interning.files);
            if !result.push_file(
                file,
                source,
                parsed.package.as_deref().unwrap_or(""),
                parsed.occurrences,
                &mut budget,
                &mut interning,
            ) {
                break;
            }
        }
        result.rebuild_search_order();
        result
    }

    /// Append one file's declarations. Returns false once the retention budget is spent, at which
    /// point the index is marked incomplete and the caller must stop.
    /// `occurrences` is extracted against `budget.remaining_entry_capacity() + 1`, so one entry
    /// past the ceiling is what tells this index it was truncated.
    fn push_file(
        &mut self,
        file: u32,
        source: &str,
        package: &str,
        mut occurrences: Vec<DocumentSymbolOccurrence>,
        budget: &mut WorkspaceSymbolBudget,
        interning: &mut WorkspaceSymbolInterning,
    ) -> bool {
        let result = self;
        let entry_capacity = budget.remaining_entry_capacity();
        let truncated = occurrences.len() > entry_capacity;
        occurrences.truncate(entry_capacity);
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
            let Some(declared) = source
                .get(occurrence.selection.lo as usize..occurrence.selection.hi as usize)
                .map(|name| name.trim_matches('`'))
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            let new_package = !interning.packages.contains_key(package);
            let new_name = !interning.names.contains_key(declared);
            if !budget.reserve_merged_entry(declared, new_name, package, new_package) {
                result.complete = false;
                result.omissions.truncated_chunks += 1;
                return false;
            }
            let package_id =
                intern_workspace_string(package, &mut result.packages, &mut interning.packages);
            let name_id =
                intern_workspace_string(declared, &mut result.names, &mut interning.names);
            let start = position(occurrence.selection.lo);
            let end = position(occurrence.selection.hi);
            let parent = parent
                .and_then(|parent| entry_offset.checked_add(parent))
                .and_then(|parent| u32::try_from(parent).ok())
                .and_then(|parent| parent.checked_add(1))
                .unwrap_or(0);
            result.entries.push([
                file,
                occurrence.selection.lo,
                occurrence.selection.hi,
                start[0],
                start[1],
                end[0],
                end[1],
                u32::from(occurrence.kind),
                parent,
                package_id,
                name_id,
                occurrence.range.lo,
                occurrence.range.hi,
            ]);
        }
        if truncated {
            result.complete = false;
            result.omissions.truncated_chunks += 1;
            return false;
        }
        true
    }

    /// Record a file skipped before parse for exceeding [`MAX_INDEXED_FILE_BYTES`].
    ///
    /// The disk adapter checks metadata before allocating a file, so an oversized file never
    /// reaches the builder at all; this explicit channel is what distinguishes the bounded
    /// omission — with the URI a user needs to act on it — from a genuinely empty chunk.
    pub(crate) fn note_oversized_file(&mut self, uri: &str, bytes: u64) {
        self.complete = false;
        self.omissions.note_oversized(uri, bytes);
    }

    #[cfg(test)]
    pub(crate) fn omissions(&self) -> &WorkspaceIndexOmissions {
        &self.omissions
    }

    pub(crate) fn take_omissions(&mut self) -> WorkspaceIndexOmissions {
        std::mem::take(&mut self.omissions)
    }

    pub fn remap_files(&mut self, remaps: &[(u32, u32)], retained_file_count: usize) {
        let mut retained = Vec::with_capacity(self.entries.len());
        let mut retained_indices = Vec::with_capacity(self.entries.len());
        for mut entry in self.entries.drain(..) {
            if let Ok(index) = remaps.binary_search_by_key(&entry[0], |(candidate, _)| *candidate) {
                entry[0] = remaps[index].1;
            }
            if entry[0] as usize >= retained_file_count {
                self.complete = false;
                retained_indices.push(None);
                continue;
            }
            entry[8] = entry[8]
                .checked_sub(1)
                .and_then(|parent| retained_indices.get(parent as usize).copied().flatten())
                .and_then(|parent: u32| parent.checked_add(1))
                .unwrap_or(0);
            let index = retained.len() as u32;
            retained.push(entry);
            retained_indices.push(Some(index));
        }
        self.entries = retained;
        self.rebuild_search_order();
    }

    /// Re-index `uris` from `replacement`, so a file re-read from disk or re-analyzed in a buffer
    /// replaces what this index held for it rather than accumulating a second copy.
    pub fn replace_files(&mut self, uris: &[String], replacement: Self) {
        self.remove_files(uris);
        self.merge_from(replacement);
    }

    /// Forget everything this index holds for `uris`.
    ///
    /// Callers pass what a producer *attempted*, not what it returned: a file it could not read is
    /// deleted or unreadable, and its stale entries have to go either way. Costs nothing when the
    /// index names none of them, which is the common case while a first sweep is still filling in.
    pub fn remove_files(&mut self, uris: &[String]) {
        if uris.is_empty() || self.files.is_empty() {
            return;
        }
        let dropped = uris.iter().map(String::as_str).collect::<HashSet<&str>>();
        let removed = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, uri)| dropped.contains(uri.as_str()))
            .map(|(id, _)| id as u32)
            .collect::<HashSet<u32>>();
        if removed.is_empty() {
            return;
        }
        self.retain_entries(|entry| !removed.contains(&entry[0]));
    }

    /// Drop every entry `keep` rejects, renumbering the parent links that survive and forgetting
    /// any file no surviving entry references.
    ///
    /// Compacting the URI table is not just housekeeping: naming a file is what makes a layer
    /// shadow the layers under it, so a file left named with nothing to say would go on hiding the
    /// copy below it.
    ///
    /// A parent always precedes its children, so a retained child's parent link either points at a
    /// retained entry or is detached to zero.
    fn retain_entries(&mut self, keep: impl Fn(&WorkspaceSymbolEntry) -> bool) {
        let mut retained = Vec::with_capacity(self.entries.len());
        let mut retained_indices = Vec::with_capacity(self.entries.len());
        let mut files = Vec::new();
        let mut file_ids = vec![None; self.files.len()];
        for mut entry in self.entries.drain(..) {
            if !keep(&entry) {
                retained_indices.push(None);
                continue;
            }
            if let Some(slot) = file_ids.get_mut(entry[0] as usize) {
                entry[0] = *slot.get_or_insert_with(|| {
                    let id = files.len() as u32;
                    files.push(std::mem::take(&mut self.files[entry[0] as usize]));
                    id
                });
            }
            entry[8] = entry[8]
                .checked_sub(1)
                .and_then(|parent| retained_indices.get(parent as usize).copied().flatten())
                .and_then(|parent: u32| parent.checked_add(1))
                .unwrap_or(0);
            let index = retained.len() as u32;
            retained.push(entry);
            retained_indices.push(Some(index));
        }
        self.entries = retained;
        self.files = files;
        // Dropping entries preserves the relative order of the ones that survive, so both rank
        // orders can be filtered and renumbered in one pass. Re-sorting them is what made
        // re-indexing a single edited file cost a pass over the whole segment holding it.
        self.retain_search_order(&retained_indices);
    }

    fn retain_search_order(&mut self, retained_indices: &[Option<u32>]) {
        for order in [&mut self.by_name, &mut self.by_initials] {
            let mut renumbered = Vec::with_capacity(order.len());
            for index in order.iter() {
                if let Some(Some(retained)) = retained_indices.get(*index as usize) {
                    renumbered.push(*retained);
                }
            }
            *order = renumbered;
        }
    }

    /// Replace positional `entry[0]` file indices with ids in the index's own URI table.
    ///
    /// `uris[position]` is the document the builder saw at that position in the analyzed source
    /// set. Only URIs an entry actually references are interned, so a source set whose support
    /// documents declared nothing costs no retained bytes. An entry whose position is past the end
    /// of `uris` describes a file this index cannot name, and is dropped as incomplete rather than
    /// kept pointing at nothing.
    ///
    /// Call once, on an index the builder just produced: binding is what ends the positional
    /// phase, and re-binding an already-bound index would read file ids as positions.
    pub fn assign_uris(&mut self, uris: &[&str]) {
        if !self.files.is_empty() {
            // A non-empty URI table proves this index is already bound. Its entry file values are
            // IDs in that table, not positions in `uris`; interpreting them a second time silently
            // moves declarations to unrelated files in the gate and release profiles where a
            // debug assertion would not exist. Preserve the valid original locations, reject the
            // phase violation through completeness, and let the caller report the shortfall.
            self.complete = false;
            return;
        }
        let mut file_ids = HashMap::<&str, u32>::new();
        let mut files = Vec::new();
        let mut retained = Vec::with_capacity(self.entries.len());
        let mut retained_indices = Vec::with_capacity(self.entries.len());
        for mut entry in self.entries.drain(..) {
            let Some(uri) = uris.get(entry[0] as usize).copied() else {
                self.complete = false;
                retained_indices.push(None);
                continue;
            };
            entry[0] = *file_ids.entry(uri).or_insert_with(|| {
                let id = files.len() as u32;
                files.push(uri.to_string());
                id
            });
            entry[8] = entry[8]
                .checked_sub(1)
                .and_then(|parent| retained_indices.get(parent as usize).copied().flatten())
                .and_then(|parent: u32| parent.checked_add(1))
                .unwrap_or(0);
            let index = retained.len() as u32;
            retained.push(entry);
            retained_indices.push(Some(index));
        }
        self.entries = retained;
        self.files = files;
        self.rebuild_search_order();
    }

    /// Every file this index can name, in file-id order.
    pub fn file_uris(&self) -> &[String] {
        &self.files
    }

    fn drop_invalid_entries(&mut self) {
        let entries = std::mem::take(&mut self.entries);
        let mut retained = Vec::with_capacity(entries.len());
        let mut retained_indices = Vec::with_capacity(entries.len());
        for mut entry in entries {
            if !workspace_symbol_entry_is_valid(&entry)
                || self.packages.get(entry[9] as usize).is_none()
                || self.names.get(entry[10] as usize).is_none()
                // An empty file table is the legitimate worker-side positional phase. Once the
                // table is non-empty, every value is a bound file id and must resolve; retaining a
                // dangling id would make queries silently skip a corrupted declaration.
                || (!self.files.is_empty() && self.files.get(entry[0] as usize).is_none())
            {
                self.complete = false;
                retained_indices.push(None);
                continue;
            }
            let parent = remap_workspace_parent(entry[8], &retained_indices).and_then(|parent| {
                let Some(parent_index) = parent.checked_sub(1) else {
                    return Some(0);
                };
                retained
                    .get(parent_index as usize)
                    .filter(|parent| workspace_symbol_parent_is_valid(parent, &entry))
                    .map(|_| parent)
            });
            if entry[8] != 0 && parent.is_none() {
                self.complete = false;
            }
            entry[8] = parent.unwrap_or(0);
            let index = retained.len() as u32;
            retained.push(entry);
            retained_indices.push(Some(index));
        }
        self.entries = retained;
    }

    fn rebuild_search_order(&mut self) {
        self.lowercase_names = self
            .names
            .iter()
            .map(|name| name.to_lowercase())
            .collect::<Vec<_>>();
        self.initials = self
            .names
            .iter()
            .map(|name| camel_hump_initials(name))
            .collect::<Vec<_>>();
        let lowercase_names = &self.lowercase_names;
        let initials = &self.initials;
        let name_id = |entry_index: u32| {
            self.entries
                .get(entry_index as usize)
                .map(|entry| entry[10] as usize)
        };
        let mut by_name = (0..self.entries.len() as u32).collect::<Vec<u32>>();
        by_name.sort_unstable_by(|left, right| {
            name_id(*left)
                .and_then(|name| lowercase_names.get(name))
                .map(String::as_str)
                .unwrap_or_default()
                .cmp(
                    name_id(*right)
                        .and_then(|name| lowercase_names.get(name))
                        .map(String::as_str)
                        .unwrap_or_default(),
                )
                .then_with(|| left.cmp(right))
        });
        let mut by_initials = (0..self.entries.len() as u32).collect::<Vec<u32>>();
        by_initials.sort_unstable_by(|left, right| {
            name_id(*left)
                .and_then(|name| initials.get(name))
                .map(String::as_str)
                .unwrap_or_default()
                .cmp(
                    name_id(*right)
                        .and_then(|name| initials.get(name))
                        .map(String::as_str)
                        .unwrap_or_default(),
                )
                .then_with(|| left.cmp(right))
        });
        self.by_name = by_name;
        self.by_initials = by_initials;
    }

    fn lowercase_name(&self, entry_index: u32) -> Option<&str> {
        let entry = self.entries.get(entry_index as usize)?;
        self.lowercase_names
            .get(entry[10] as usize)
            .map(String::as_str)
    }

    fn source_name(&self, entry_index: u32) -> Option<&str> {
        let entry = self.entries.get(entry_index as usize)?;
        self.names.get(entry[10] as usize).map(String::as_str)
    }

    fn entry_initials(&self, entry_index: u32) -> &str {
        self.entries
            .get(entry_index as usize)
            .and_then(|entry| self.initials.get(entry[10] as usize))
            .map(String::as_str)
            .unwrap_or_default()
    }

    fn prefix_matches(&self, lowercase_query: &str) -> &[u32] {
        let start = self.by_name.partition_point(|&index| {
            self.lowercase_name(index).unwrap_or_default() < lowercase_query
        });
        let count = self.by_name[start..].partition_point(|&index| {
            self.lowercase_name(index)
                .unwrap_or_default()
                .starts_with(lowercase_query)
        });
        &self.by_name[start..start + count]
    }

    fn initials_matches(&self, lowercase_query: &str) -> &[u32] {
        let start = self
            .by_initials
            .partition_point(|&index| self.entry_initials(index) < lowercase_query);
        let count = self.by_initials[start..]
            .partition_point(|&index| self.entry_initials(index).starts_with(lowercase_query));
        &self.by_initials[start..start + count]
    }

    fn retain_wire_budget(&mut self, max_wire_bytes: usize) {
        if self.wire_bytes() <= max_wire_bytes {
            return;
        }
        self.retain_entries_within(max_wire_bytes);
    }

    /// Retain against the conservative accounting used by the segmented project layer. JSON wire
    /// size is intentionally not the trigger here: `retained_wire_bytes` also reserves both search
    /// permutations at their worst-case integer width, and mixing those two measures would let the
    /// aggregate exceed the ceiling even though each segment passed its own exact-wire check.
    fn retain_accounted_wire_budget(&mut self, max_wire_bytes: usize) {
        if self.retained_wire_bytes() <= max_wire_bytes {
            return;
        }
        self.retain_entries_within(max_wire_bytes);
    }

    fn retain_entries_within(&mut self, max_wire_bytes: usize) {
        let old = std::mem::take(self);
        let old_entry_count = old.entries.len();
        let mut retained = Self {
            complete: false,
            omissions: old.omissions.clone(),
            ..Self::default()
        };
        let mut budget = WorkspaceSymbolBudget::with_limit(max_wire_bytes);
        let mut package_ids = HashMap::<String, u32>::new();
        let mut name_ids = HashMap::<String, u32>::new();
        let mut file_ids = HashMap::<String, u32>::new();
        let mut retained_indices = Vec::with_capacity(old.entries.len());
        for mut entry in old.entries {
            let Some(package) = old.packages.get(entry[9] as usize) else {
                retained_indices.push(None);
                continue;
            };
            let Some(name) = old.names.get(entry[10] as usize) else {
                retained_indices.push(None);
                continue;
            };
            let uri = match old.files.get(entry[0] as usize) {
                Some(uri) => Some(uri.as_str()),
                None if old.files.is_empty() => None,
                None => {
                    retained_indices.push(None);
                    continue;
                }
            };
            let Some(parent) = remap_workspace_parent(entry[8], &retained_indices) else {
                retained_indices.push(None);
                continue;
            };
            let new_package = !package_ids.contains_key(package);
            let new_name = !name_ids.contains_key(name);
            let new_file = uri.is_some_and(|uri| !file_ids.contains_key(uri));
            if !budget.reserve_merged_entry_in_file(
                name,
                new_name,
                package,
                new_package,
                uri.unwrap_or(""),
                new_file,
            ) {
                break;
            }
            entry[8] = parent;
            if let Some(uri) = uri {
                entry[0] = intern_workspace_string(uri, &mut retained.files, &mut file_ids);
            }
            entry[9] = intern_workspace_string(package, &mut retained.packages, &mut package_ids);
            entry[10] = intern_workspace_string(name, &mut retained.names, &mut name_ids);
            let index = retained.entries.len() as u32;
            retained.entries.push(entry);
            retained_indices.push(Some(index));
        }
        retained.omissions.dropped_entries += old_entry_count - retained.entries.len();
        retained.rebuild_search_order();
        *self = retained;
    }

    fn wire_bytes(&self) -> usize {
        serialized_json_wire_bytes(self).map_or(usize::MAX, |bytes| bytes.saturating_add(1))
    }

    pub fn merge_from(&mut self, other: Self) {
        self.merge_within(other, MAX_SOURCE_SET_WORKSPACE_SYMBOL_INDEX_WIRE_BYTES);
    }

    /// Merge under an explicit retention ceiling. The source-set default bounds one worker message;
    /// the project-wide index is retained by the session and gets its own, larger, ceiling.
    pub fn merge_within(&mut self, mut other: Self, max_wire_bytes: usize) {
        let self_is_bound = !self.files.is_empty();
        let other_is_bound = !other.files.is_empty();
        if !self.entries.is_empty() && !other.entries.is_empty() && self_is_bound != other_is_bound
        {
            // `entry[0]` has two meanings across the worker boundary: a source position before
            // `assign_uris`, and a file-table id afterwards. Merging either non-empty phase into
            // the other cannot be repaired by choosing one interpretation; both commonly start at
            // zero. Refuse symmetrically and retain the target's still-valid entries. Empty indexes
            // are phase-agnostic and remain useful as merge accumulators/tombstones.
            self.complete = false;
            // The refusal discards `other` wholesale: keep the provenance it carried and count
            // its entries, or the client log has no cause for the resulting incompleteness.
            self.omissions.absorb(std::mem::take(&mut other.omissions));
            self.omissions.dropped_entries += other.entries.len();
            return;
        }
        let mut budget = WorkspaceSymbolBudget::from_index_within(self, max_wire_bytes);
        let mut package_ids = self
            .packages
            .iter()
            .enumerate()
            .map(|(index, value)| (value.clone(), index as u32))
            .collect::<HashMap<_, _>>();
        let mut name_ids = self
            .names
            .iter()
            .enumerate()
            .map(|(index, value)| (value.clone(), index as u32))
            .collect::<HashMap<_, _>>();
        let mut file_ids = self
            .files
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
        self.complete &= other.complete;
        self.omissions.absorb(std::mem::take(&mut other.omissions));
        let other_entry_count = other.entries.len();
        let mut processed = 0usize;
        let mut remapped_entries = Vec::with_capacity(other.entries.len());
        for mut entry in other.entries {
            processed += 1;
            // A bound index names its files; an unbound one still numbers them by position and
            // shares that numbering with the index it is merging into. Either way `entry[0]` has to
            // reach its final value before the identity check, or the same declaration merged from
            // two source sets would not recognise itself.
            let uri = match other.files.get(entry[0] as usize) {
                Some(uri) => Some(uri.as_str()),
                // A positional index shares its numbering with the index it merges into, so this
                // is only meaningful while neither side has bound its URIs. Reading a file id as a
                // position would silently report declarations against the wrong file, and the
                // debug assertion below compiles out of both the gate and release profiles, so
                // refuse the entry rather than trust it.
                None if other.files.is_empty() && self.files.is_empty() => None,
                None => {
                    debug_assert!(
                        other.files.is_empty(),
                        "workspace symbol entry names a file its index does not hold"
                    );
                    self.complete = false;
                    remapped_entries.push(None);
                    continue;
                }
            };
            let known_file = uri.and_then(|uri| file_ids.get(uri).copied());
            if let Some(file) = known_file {
                entry[0] = file;
            }
            if uri.is_none() || known_file.is_some() {
                if let Some(&index) = identities.get(&workspace_symbol_identity(&entry)) {
                    remapped_entries.push(Some(index));
                    continue;
                }
            }
            let Some(package) = other.packages.get(entry[9] as usize) else {
                self.complete = false;
                remapped_entries.push(None);
                continue;
            };
            let Some(name) = other.names.get(entry[10] as usize) else {
                self.complete = false;
                remapped_entries.push(None);
                continue;
            };
            let new_package = !package_ids.contains_key(package);
            let new_name = !name_ids.contains_key(name);
            if !budget.reserve_merged_entry_in_file(
                name,
                new_name,
                package,
                new_package,
                uri.unwrap_or(""),
                uri.is_some() && known_file.is_none(),
            ) {
                self.complete = false;
                // This entry and every unprocessed one behind it fall out of retention here.
                self.omissions.dropped_entries += other_entry_count - processed + 1;
                break;
            }
            if let Some(uri) = uri {
                entry[0] = intern_workspace_string(uri, &mut self.files, &mut file_ids);
            }
            let package_id = intern_workspace_string(package, &mut self.packages, &mut package_ids);
            let name_id = intern_workspace_string(name, &mut self.names, &mut name_ids);
            entry[8] = entry[8]
                .checked_sub(1)
                .and_then(|parent| remapped_entries.get(parent as usize).copied().flatten())
                .and_then(|parent| parent.checked_add(1))
                .unwrap_or(0);
            entry[9] = package_id;
            entry[10] = name_id;
            let index = self.entries.len() as u32;
            identities.insert(workspace_symbol_identity(&entry), index);
            self.entries.push(entry);
            remapped_entries.push(Some(index));
        }
        self.rebuild_search_order();
    }

    pub fn encode(&self, query: &str) -> Vec<Value> {
        Self::encode_layers(query, &[self], &HashSet::new())
    }

    /// Answer a query from this index layered over `base`.
    ///
    /// This index is the live one -- open buffers and the sources analysis pulled in with them --
    /// and `base` is what lies under it, outermost layer first.
    ///
    /// `open` is every document the session holds a buffer for. Shadowing is the union of that and
    /// the files the live layer names, and it needs both: the live layer alone misses a buffer
    /// edited down to no declarations at all, which would leave the project layer answering with
    /// the file's text from disk, and `open` alone misses the support sources analysis pulled in
    /// beside the open ones, which would report those declarations from two layers at once.
    pub fn encode_over(&self, query: &str, base: &[&Self], open: &HashSet<&str>) -> Vec<Value> {
        let mut remaining_glob_steps = MAX_WORKSPACE_SYMBOL_GLOB_STEPS;
        self.encode_over_with_glob_steps(query, base, open, &mut remaining_glob_steps)
    }

    /// Encode with a request-owned wildcard transition budget.
    ///
    /// The LSP service passes the remainder to the dependency layer after project encoding. This
    /// is intentionally separate from the ordinary convenience entry point: the budget covers the
    /// composed protocol request, not each retained index independently.
    pub(crate) fn encode_over_with_glob_steps(
        &self,
        query: &str,
        base: &[&Self],
        open: &HashSet<&str>,
        remaining_glob_steps: &mut usize,
    ) -> Vec<Value> {
        let mut layers = Vec::with_capacity(base.len() + 1);
        layers.push(self);
        layers.extend(base.iter().copied());
        Self::encode_layers_with_glob_steps(query, &layers, open, remaining_glob_steps)
    }

    /// Rank across every layer at once, rung by rung.
    ///
    /// Layer order decides shadowing, not rank: running a whole layer's ladder before the next
    /// layer's would let a subsequence match in a newly indexed chunk outrank an exact prefix match
    /// in the segment beside it, and spend the response budget on it.
    fn encode_layers(query: &str, layers: &[&Self], open: &HashSet<&str>) -> Vec<Value> {
        let mut remaining_glob_steps = MAX_WORKSPACE_SYMBOL_GLOB_STEPS;
        Self::encode_layers_with_glob_steps(query, layers, open, &mut remaining_glob_steps)
    }

    /// Budgeted query implementation. The transition budget belongs to the request, not an index
    /// segment: a project query walks every layer and may also try a keyboard-layout translation,
    /// so resetting inside either loop multiplies attacker-controlled work by both dimensions.
    fn encode_layers_with_glob_steps(
        query: &str,
        layers: &[&Self],
        open: &HashSet<&str>,
        remaining_glob_steps: &mut usize,
    ) -> Vec<Value> {
        let mut result = Vec::new();
        // Parsing, input bounds, keyboard-layout translation, and rung selection are shared with
        // the dependency index. They are protocol semantics, not storage-layer policy: letting a
        // second index reinterpret the same request previously made the dependency path bypass the
        // workspace query's denial-of-service bounds and disagree about wildcard syntax.
        let parsed = workspace_queries(query);

        let mut shadowed = open.clone();
        let mut suppressed = Vec::with_capacity(layers.len());
        for (index, layer) in layers.iter().enumerate() {
            // The outermost layer answers for its own files; it is what everything below shadows
            // against, not something to shadow itself.
            suppressed.push(if index == 0 {
                HashSet::new()
            } else {
                layer.shadowed_files(&shadowed)
            });
            shadowed.extend(layer.files.iter().map(String::as_str));
        }

        let mut wire_bytes = 2usize;
        // Two query forms can reach the same entry, so ranks are deduplicated across them.
        let mut seen = vec![HashSet::new(); layers.len()];
        for query in &parsed {
            for rung in query.rungs() {
                for (index, layer) in layers.iter().enumerate() {
                    let scope = EncodeScope {
                        query,
                        suppressed: &suppressed[index],
                    };
                    if !layer.encode_rung(
                        *rung,
                        &scope,
                        &mut result,
                        &mut wire_bytes,
                        &mut seen[index],
                        remaining_glob_steps,
                    ) {
                        return result;
                    }
                }
            }
        }
        result
    }

    /// File ids in this index whose URI a higher layer already answers for.
    fn shadowed_files(&self, shadowed: &HashSet<&str>) -> HashSet<u32> {
        if shadowed.is_empty() {
            return HashSet::new();
        }
        self.files
            .iter()
            .enumerate()
            .filter(|(_, uri)| shadowed.contains(uri.as_str()))
            .map(|(id, _)| id as u32)
            .collect()
    }

    /// Which interned names satisfy a fallback rung.
    ///
    /// Both fallback rungs are scans, and a project-wide index holds several entries per distinct
    /// name -- 698,516 declarations over 173,551 names on the reference corpus. Testing the name
    /// table once and reducing the per-entry work to an array read is what keeps a scan affordable
    /// on the request thread.
    /// `predicate` receives the name already lowercased and its camel-hump initials, both cached,
    /// so a rung never lowercases or re-derives anything per name.
    fn matching_names(&self, predicate: impl Fn(&str, &str) -> bool) -> Vec<bool> {
        self.lowercase_names
            .iter()
            .enumerate()
            .map(|(id, name)| {
                let initials = self
                    .initials
                    .get(id)
                    .map(String::as_str)
                    .unwrap_or_default();
                predicate(name, initials)
            })
            .collect()
    }

    fn name_matches(&self, entry_index: u32, matching: &[bool]) -> bool {
        self.entries
            .get(entry_index as usize)
            .and_then(|entry| matching.get(entry[10] as usize))
            .copied()
            .unwrap_or(false)
    }

    /// Encode one rung of the ladder. Returns false once the wire budget is spent.
    fn encode_rung(
        &self,
        rung: WorkspaceSymbolRung,
        scope: &EncodeScope<'_>,
        result: &mut Vec<Value>,
        wire_bytes: &mut usize,
        seen: &mut HashSet<u32>,
        remaining_glob_steps: &mut usize,
    ) -> bool {
        let query = scope.query;
        let lowercase_query = &query.pattern;
        match rung {
            WorkspaceSymbolRung::Every => {
                for index in 0..self.entries.len() as u32 {
                    if !self.admit(index, scope, result, wire_bytes, seen) {
                        return false;
                    }
                }
            }
            WorkspaceSymbolRung::Glob => {
                // A literal prefix still narrows through the sorted array; `*foo*` has none and
                // falls back to a scan, which is the cost of a leading wildcard.
                let prefix = query.literal_prefix();
                return if prefix.is_empty() {
                    self.encode_glob_candidates(
                        0..self.entries.len() as u32,
                        scope,
                        result,
                        wire_bytes,
                        seen,
                        remaining_glob_steps,
                    )
                } else {
                    self.encode_glob_candidates(
                        self.prefix_matches(prefix).iter().copied(),
                        scope,
                        result,
                        wire_bytes,
                        seen,
                        remaining_glob_steps,
                    )
                };
            }
            WorkspaceSymbolRung::NamePrefix => {
                for &index in self.prefix_matches(lowercase_query) {
                    if !self.admit(index, scope, result, wire_bytes, seen) {
                        return false;
                    }
                }
            }
            WorkspaceSymbolRung::InitialsPrefix => {
                for &index in self.initials_matches(lowercase_query) {
                    if self
                        .source_name(index)
                        .is_some_and(|name| starts_with_lowercase(name, lowercase_query))
                    {
                        continue;
                    }
                    if !self.admit(index, scope, result, wire_bytes, seen) {
                        return false;
                    }
                }
            }
            WorkspaceSymbolRung::InitialsSubsequence => {
                // The rung's own condition is the selective one; the exclusions only decide
                // whether an earlier rung already claimed the name, so test them second.
                let matching = self.matching_names(|name, initials| {
                    is_ordered_subsequence_lowercase(initials, lowercase_query)
                        && !initials.starts_with(lowercase_query)
                        && !name.starts_with(lowercase_query)
                });
                for &index in &self.by_initials {
                    if !self.name_matches(index, &matching) {
                        continue;
                    }
                    if !self.admit(index, scope, result, wire_bytes, seen) {
                        return false;
                    }
                }
            }
            WorkspaceSymbolRung::NameSubsequence => {
                let matching = self.matching_names(|name, initials| {
                    is_ordered_subsequence_lowercase(name, lowercase_query)
                        && !name.starts_with(lowercase_query)
                        && !is_ordered_subsequence_lowercase(initials, lowercase_query)
                });
                for index in 0..self.entries.len() as u32 {
                    if !self.name_matches(index, &matching) {
                        continue;
                    }
                    if !self.admit(index, scope, result, wire_bytes, seen) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Verify a candidate range without materialising another entry-index vector. `None` from the
    /// matcher means the per-request transition budget is exhausted; return the bounded prefix
    /// already encoded rather than continuing an attacker-controlled backtracking scan.
    fn encode_glob_candidates(
        &self,
        candidates: impl IntoIterator<Item = u32>,
        scope: &EncodeScope<'_>,
        result: &mut Vec<Value>,
        wire_bytes: &mut usize,
        seen: &mut std::collections::HashSet<u32>,
        remaining_steps: &mut usize,
    ) -> bool {
        for index in candidates {
            let Some(name) = self.source_name(index) else {
                continue;
            };
            match matches_glob(&name.to_lowercase(), &scope.query.pattern, remaining_steps) {
                Some(true) => {}
                Some(false) => continue,
                None => return false,
            }
            if !self.admit(index, scope, result, wire_bytes, seen) {
                return false;
            }
        }
        true
    }

    /// Encode one entry if a higher layer does not already answer for its file, its package
    /// satisfies the query, and no earlier rung claimed it.
    fn admit(
        &self,
        index: u32,
        scope: &EncodeScope<'_>,
        result: &mut Vec<Value>,
        wire_bytes: &mut usize,
        seen: &mut std::collections::HashSet<u32>,
    ) -> bool {
        if self
            .entries
            .get(index as usize)
            .is_some_and(|entry| scope.suppressed.contains(&entry[0]))
        {
            return true;
        }
        if !self.package_matches(index, scope.query.package.as_deref()) {
            return true;
        }
        if !seen.insert(index) {
            return true;
        }
        self.push_encoded(index, scope.query, result, wire_bytes)
    }

    /// Whether an entry's package satisfies a qualified query. An unqualified query admits every
    /// entry; a qualified one matches a complete package suffix on a segment boundary, so
    /// `collections.listOf` finds `kotlin.collections` without also admitting
    /// `kotlin.collectionsExtra`.
    fn package_matches(&self, entry_index: u32, package: Option<&str>) -> bool {
        let Some(package) = package else {
            return true;
        };
        let Some(entry) = self.entries.get(entry_index as usize) else {
            return false;
        };
        self.packages
            .get(entry[9] as usize)
            .is_some_and(|declared| {
                let declared = declared.to_lowercase();
                declared == package
                    || declared
                        .strip_suffix(package)
                        .is_some_and(|prefix| prefix.ends_with('.'))
            })
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// What this index costs against a retention budget: its entries plus every string table.
    fn retained_wire_bytes(&self) -> usize {
        self.entries
            .len()
            .saturating_mul(WORKSPACE_SYMBOL_ENTRY_MAX_WIRE_BYTES)
            .saturating_add(
                self.packages
                    .iter()
                    .chain(&self.names)
                    .chain(&self.files)
                    .fold(0usize, |bytes, value| {
                        bytes.saturating_add(workspace_symbol_string_wire_cost(value))
                    }),
            )
    }

    /// Whether every indexable declaration fit in the retained snapshot budget.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    fn clear_incomplete(&mut self) {
        *self = Self {
            complete: false,
            ..Self::default()
        };
    }

    fn push_encoded(
        &self,
        entry_index: u32,
        query: &WorkspaceQuery,
        result: &mut Vec<Value>,
        wire_bytes: &mut usize,
    ) -> bool {
        let Some(entry) = self.entries.get(entry_index as usize).copied() else {
            return true;
        };
        let Some(uri) = self.files.get(entry[0] as usize) else {
            return true;
        };
        let Some(source_name) = self.source_name(entry_index) else {
            return true;
        };
        // Zed re-filters workspace symbols against the returned `name`. A package-qualified query
        // cannot survive that filter if the server returns only the declaration's bare name, so
        // preserve the user's separator and qualify only these responses. Unqualified pickers keep
        // the compact source spelling.
        let name = query.response_name(
            self.packages
                .get(entry[9] as usize)
                .map(String::as_str)
                .unwrap_or(""),
            source_name,
        );
        let symbol = json!({
            "name": name,
            "kind": entry[7],
            "containerName": self.container_name(entry_index as usize),
            "location": {
                "uri": uri,
                "range": {
                    "start": {"line": entry[3], "character": entry[4]},
                    "end": {"line": entry[5], "character": entry[6]},
                },
            },
        });
        let symbol_bytes = serialized_json_wire_bytes(&symbol).unwrap_or(usize::MAX);
        let next_bytes = wire_bytes.saturating_add(symbol_bytes).saturating_add(1);
        if next_bytes > MAX_WORKSPACE_SYMBOL_WIRE_BYTES
            || result.len() >= MAX_WORKSPACE_SYMBOL_RESPONSE_SYMBOLS
        {
            return false;
        }
        *wire_bytes = next_bytes;
        result.push(symbol);
        true
    }

    fn container_name(&self, entry_index: usize) -> String {
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
            let Some(name) = self
                .names
                .get(parent_entry[10] as usize)
                .map(String::as_str)
            else {
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

fn remap_workspace_parent(parent: u32, retained_indices: &[Option<u32>]) -> Option<u32> {
    match parent.checked_sub(1) {
        Some(parent) => retained_indices
            .get(parent as usize)
            .copied()
            .flatten()?
            .checked_add(1),
        None => Some(0),
    }
}

fn workspace_symbol_entry_is_valid(entry: &WorkspaceSymbolEntry) -> bool {
    entry[1] <= entry[2]
        && entry[11] <= entry[1]
        && entry[2] <= entry[12]
        && entry[11] <= entry[12]
        && (entry[3], entry[4]) <= (entry[5], entry[6])
}

fn workspace_symbol_parent_is_valid(
    parent: &WorkspaceSymbolEntry,
    child: &WorkspaceSymbolEntry,
) -> bool {
    parent[0] == child[0]
        && parent[9] == child[9]
        && parent[11] <= child[11]
        && child[12] <= parent[12]
        && (parent[11], parent[12]) != (child[11], child[12])
}

/// A parsed `workspace/symbol` query.
///
/// The protocol defines no matching semantics beyond "a query string to filter symbols by", so the
/// grammar here is ours. Zed re-filters results with a subsequence matcher against the name the
/// server returns, which silently discards anything whose query contains characters no symbol name
/// holds - wildcards, or a wrong-layout query. A trailing `::` is the escape: Zed keeps only the text
/// after the last `::` for its own filter, so an empty remainder disables client filtering and leaves
/// the server authoritative.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceQuery {
    /// Folded name pattern; may contain `*` and `?` when `globbed` is set.
    pub(crate) pattern: String,
    /// Folded package prefix, when the query was written qualified (`kotlin.collections.listOf`).
    pub(crate) package: Option<String>,
    /// Separator used by the client. Qualified response names preserve it so a client that
    /// re-filters the returned text can match both dotted and slashed queries.
    separator: Option<char>,
    globbed: bool,
}

impl WorkspaceQuery {
    pub(crate) fn parse(raw: &str) -> WorkspaceQuery {
        // Everything before the last `::` is the server-side query; the marker itself is not matched.
        let body = raw.rsplit_once("::").map_or(
            raw,
            |(head, tail)| {
                if tail.is_empty() {
                    head
                } else {
                    raw
                }
            },
        );
        let folded = body.to_lowercase();
        // A dotted or slashed query names a package; the last segment is the symbol. Retain the
        // delimiter separately because the response name must survive client-side filtering.
        let (package, pattern, separator) = match folded.rfind(['.', '/']) {
            Some(index) if index > 0 && index + 1 < folded.len() => {
                let separator = folded[index..].chars().next().expect("ASCII separator");
                (
                    Some(folded[..index].replace('/', ".")),
                    folded[index + 1..].to_string(),
                    Some(separator),
                )
            }
            _ => (None, folded, None),
        };
        WorkspaceQuery {
            globbed: pattern.contains('*') || pattern.contains('?'),
            pattern,
            package,
            separator,
        }
    }

    /// The literal text before the first wildcard, usable as a sorted-array prefix.
    pub(crate) fn literal_prefix(&self) -> &str {
        let end = self.pattern.find(['*', '?']).unwrap_or(self.pattern.len());
        &self.pattern[..end]
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pattern.is_empty() && self.package.is_none()
    }

    /// The common ranking ladder for this query shape.
    ///
    /// Kept on the parsed query so every workspace-symbol storage layer makes the same choice for
    /// empty, wildcard, and ordinary requests. The layers still decide whether an empty query is
    /// appropriate for their scope; the dependency layer deliberately declines it.
    pub(crate) fn rungs(&self) -> &'static [WorkspaceSymbolRung] {
        if self.is_empty() {
            &[WorkspaceSymbolRung::Every]
        } else if self.globbed {
            &[WorkspaceSymbolRung::Glob]
        } else {
            &[
                WorkspaceSymbolRung::NamePrefix,
                WorkspaceSymbolRung::InitialsPrefix,
                WorkspaceSymbolRung::InitialsSubsequence,
                WorkspaceSymbolRung::NameSubsequence,
            ]
        }
    }

    pub(crate) fn response_name(&self, declared_package: &str, source_name: &str) -> String {
        let Some(separator) = self.separator else {
            return source_name.to_string();
        };
        if declared_package.is_empty() {
            return source_name.to_string();
        }
        let package = if separator == '/' {
            declared_package.replace('.', "/")
        } else {
            declared_package.to_string()
        };
        format!("{package}{separator}{source_name}")
    }
}

/// Parse every spelling of one workspace-symbol query that the server searches.
///
/// The byte ceiling belongs here rather than in an individual index. A request is interpreted once
/// for all layers, including the positional Cyrillic-to-Latin fallback, so adding another symbol
/// source cannot accidentally create an unbounded or semantically different request path.
pub(crate) fn workspace_queries(raw: &str) -> Vec<WorkspaceQuery> {
    if raw.len() > MAX_WORKSPACE_SYMBOL_QUERY_BYTES {
        return Vec::new();
    }
    let mut parsed = vec![WorkspaceQuery::parse(raw)];
    if let Some(latin) = qwerty_from_cyrillic(raw) {
        let translated = WorkspaceQuery::parse(&latin);
        if translated != parsed[0] {
            parsed.push(translated);
        }
    }
    parsed
}

/// Glob match over folded text: `*` spans any run, `?` one character.
pub(crate) fn matches_glob(text: &str, pattern: &str, remaining_steps: &mut usize) -> Option<bool> {
    // Keep byte offsets only for slicing, but advance them by decoded characters. Treating `?` as
    // one UTF-8 byte made it impossible to match the Unicode identifiers Kotlin permits.
    let (mut text_offset, mut pattern_offset) = (0usize, 0usize);
    let mut star_pattern = None;
    let mut star_text = 0usize;
    while text_offset < text.len() {
        *remaining_steps = remaining_steps.checked_sub(1)?;
        let (text_character, next_text) = next_character(text, text_offset)?;
        match next_character(pattern, pattern_offset) {
            Some(('*', next_pattern)) => {
                star_pattern = Some(next_pattern);
                star_text = text_offset;
                pattern_offset = next_pattern;
            }
            Some((candidate, next_pattern)) if candidate == '?' || candidate == text_character => {
                text_offset = next_text;
                pattern_offset = next_pattern;
            }
            _ => {
                let Some(resume_pattern) = star_pattern else {
                    return Some(false);
                };
                let Some((_, next_resume)) = next_character(text, star_text) else {
                    return Some(false);
                };
                star_text = next_resume;
                text_offset = next_resume;
                pattern_offset = resume_pattern;
            }
        }
    }
    while let Some((character, next_pattern)) = next_character(pattern, pattern_offset) {
        *remaining_steps = remaining_steps.checked_sub(1)?;
        if character != '*' {
            return Some(false);
        }
        pattern_offset = next_pattern;
    }
    Some(true)
}

fn next_character(value: &str, offset: usize) -> Option<(char, usize)> {
    let character = value.get(offset..)?.chars().next()?;
    Some((character, offset.saturating_add(character.len_utf8())))
}

/// Positional ЙЦУКЕН -> QWERTY mapping, for a query typed without switching layout.
///
/// Returns `None` when the query holds no Cyrillic, so callers can skip the second search.
pub(crate) fn qwerty_from_cyrillic(query: &str) -> Option<String> {
    let mut mapped = String::with_capacity(query.len());
    let mut translated = false;
    for character in query.chars() {
        let folded = character.to_lowercase().next().unwrap_or(character);
        match qwerty_from_cyrillic_character(folded) {
            Some(latin) => {
                mapped.push(latin);
                translated = true;
            }
            None => mapped.push(character),
        }
    }
    translated.then_some(mapped)
}

fn qwerty_from_cyrillic_character(character: char) -> Option<char> {
    Some(match character {
        'й' => 'q',
        'ц' => 'w',
        'у' => 'e',
        'к' => 'r',
        'е' => 't',
        'н' => 'y',
        'г' => 'u',
        'ш' => 'i',
        'щ' => 'o',
        'з' => 'p',
        'х' => '[',
        'ъ' => ']',
        'ф' => 'a',
        'ы' => 's',
        'в' => 'd',
        'а' => 'f',
        'п' => 'g',
        'р' => 'h',
        'о' => 'j',
        'л' => 'k',
        'д' => 'l',
        'ж' => ';',
        'э' => '\'',
        'я' => 'z',
        'ч' => 'x',
        'с' => 'c',
        'м' => 'v',
        'и' => 'b',
        'т' => 'n',
        'ь' => 'm',
        'б' => ',',
        'ю' => '.',
        'ё' => '`',
        _ => return None,
    })
}

pub(crate) fn camel_hump_initials(name: &str) -> String {
    let mut initials = String::new();
    let mut chars = name.chars().peekable();
    let mut previous = None;
    while let Some(ch) = chars.next() {
        let next = chars.peek().copied();
        let boundary = previous.is_none()
            || previous == Some('_')
            || (ch.is_uppercase()
                && (previous.is_some_and(|previous| !previous.is_uppercase())
                    || next.is_some_and(char::is_lowercase)));
        if boundary && ch.is_alphanumeric() {
            initials.extend(ch.to_lowercase());
        }
        previous = Some(ch);
    }
    initials
}

pub(crate) fn is_ordered_subsequence_lowercase(haystack: &str, lowercase_needle: &str) -> bool {
    if lowercase_needle.is_empty() {
        return true;
    }
    if haystack.is_ascii() && lowercase_needle.is_ascii() {
        let mut needle = lowercase_needle.bytes();
        let Some(mut expected) = needle.next() else {
            return true;
        };
        for byte in haystack.bytes() {
            if byte.to_ascii_lowercase() != expected {
                continue;
            }
            let Some(next) = needle.next() else {
                return true;
            };
            expected = next;
        }
        return false;
    }
    let mut needle = lowercase_needle.chars();
    let Some(mut expected) = needle.next() else {
        return true;
    };
    for ch in haystack.chars().flat_map(char::to_lowercase) {
        if ch != expected {
            continue;
        }
        let Some(next) = needle.next() else {
            return true;
        };
        expected = next;
    }
    false
}

fn starts_with_lowercase(haystack: &str, lowercase_needle: &str) -> bool {
    if !haystack.is_ascii() || !lowercase_needle.is_ascii() {
        return haystack.to_lowercase().starts_with(lowercase_needle);
    }
    haystack
        .as_bytes()
        .get(..lowercase_needle.len())
        .is_some_and(|prefix| {
            prefix
                .iter()
                .zip(lowercase_needle.as_bytes())
                .all(|(left, right)| left.to_ascii_lowercase() == *right)
        })
}

fn intern_workspace_string(
    value: &str,
    values: &mut Vec<String>,
    ids: &mut HashMap<String, u32>,
) -> u32 {
    if let Some(&id) = ids.get(value) {
        return id;
    }
    let id = values.len() as u32;
    let value = value.to_string();
    ids.insert(value.clone(), id);
    values.push(value);
    id
}

fn workspace_symbol_string_wire_cost(value: &str) -> usize {
    value.len().saturating_mul(6).saturating_add(3)
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

/// One chunk's indexing result.
///
/// `conclusive` separates "the analysis ran and these files produced nothing" from "the analysis
/// could not run". Only the former may delete retained data; conflating them lets a worker restart
/// silently erase a whole chunk's diagnostics.
#[derive(Clone, Debug, Default)]
pub struct IndexOutcome {
    pub files: Vec<IndexedFile>,
    pub conclusive: bool,
}

/// One workspace file's indexing result. Only diagnostics and the text hash are retained; the
/// rich per-document indices are derived while indexing and dropped, because a swept file is
/// re-analysed interactively the moment it is opened.
#[derive(Clone, Debug)]
pub struct IndexedFile {
    pub uri: String,
    pub diagnostics: Vec<Diagnostic>,
    pub text_hash: u64,
    /// Retained only until the store resolves byte spans to line and UTF-16 column, then dropped.
    /// Resolving later would need the text again, and re-reading would race the sweep.
    pub text: String,
}

/// Global ceiling on one model generation's cached source inventory.
pub const MAX_WORKSPACE_INDEX_FILES: usize = 200_000;
/// URI payload and record bytes retained by the producer's cached source inventory.
pub const MAX_WORKSPACE_INDEX_URI_BYTES: usize = 16 * 1024 * 1024;

/// One owned URI's payload plus its `String` record. Queue layers multiply this by the number of
/// representations they retain, keeping producer and consumer accounting on the same unit.
pub fn workspace_index_uri_bytes(uri: &str) -> usize {
    uri.len().saturating_add(std::mem::size_of::<String>())
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
                &analysis.file,
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

    pub fn remap_navigation_files(&mut self, remaps: &[(u32, u32)], retained_file_count: usize) {
        self.definitions.remap_files(remaps);
        self.type_definitions.remap_files(remaps);
        self.implementations.remap_files(remaps);
        self.workspace_symbols
            .remap_files(remaps, retained_file_count);
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
        ANALYSIS_RESPONSE_FIXED_WIRE_BYTES
            .saturating_add(self.diagnostic_wire_bytes())
            .saturating_add(self.non_workspace_semantic_wire_bytes())
            .saturating_add(self.workspace_symbol_wire_bytes())
    }

    fn diagnostic_wire_bytes(&self) -> usize {
        self.diagnostics.iter().fold(0usize, |bytes, diagnostic| {
            bytes
                .saturating_add(96)
                .saturating_add(diagnostic.msg.len().saturating_mul(6))
        })
    }

    fn non_workspace_semantic_wire_bytes(&self) -> usize {
        serialized_json_wire_bytes(&(
            &self.hover,
            &self.completion,
            &self.signature_help,
            &self.semantic_tokens,
            &self.definitions,
            &self.type_definitions,
            &self.implementations,
            &self.library_definitions,
            &self.document_symbols,
            &self.folding_ranges,
            &self.implementation_relations,
        ))
        .unwrap_or(usize::MAX)
    }

    fn workspace_symbol_wire_bytes(&self) -> usize {
        self.workspace_symbols.wire_bytes()
    }

    fn clear_non_workspace_semantic_indexes(&mut self) {
        self.hover = HoverIndex::default();
        self.completion = CompletionIndex::default();
        self.signature_help = SignatureHelpIndex::default();
        self.semantic_tokens = SemanticTokenIndex::default();
        self.definitions = DefinitionIndex::default();
        self.type_definitions = DefinitionIndex::default();
        self.implementations = DefinitionIndex::default();
        self.library_definitions = LibraryDefinitionIndex::default();
        self.document_symbols = DocumentSymbolIndex::default();
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
    let mut empty = DocumentAnalysis::empty();
    empty.workspace_symbols.clear_incomplete();
    let non_workspace_floor = empty.non_workspace_semantic_wire_bytes();
    let workspace_floor = empty.workspace_symbol_wire_bytes();
    let retained_floor = ANALYSIS_RESPONSE_FIXED_WIRE_BYTES
        .saturating_add(non_workspace_floor)
        .saturating_add(workspace_floor)
        .saturating_mul(analyses.len());
    let mut remaining = max_bytes.saturating_sub(retained_floor);

    for analysis in analyses.iter_mut() {
        let mut diagnostic_bytes = analysis.diagnostic_wire_bytes();
        while diagnostic_bytes > remaining && !analysis.diagnostics.is_empty() {
            let diagnostic = analysis.diagnostics.pop().unwrap();
            diagnostic_bytes = diagnostic_bytes
                .saturating_sub(96usize.saturating_add(diagnostic.msg.len().saturating_mul(6)));
        }
        remaining = remaining.saturating_sub(diagnostic_bytes);
    }

    for analysis in analyses.iter_mut() {
        let semantic_bytes = analysis.non_workspace_semantic_wire_bytes();
        let additional_bytes = semantic_bytes.saturating_sub(non_workspace_floor);
        if additional_bytes <= remaining {
            remaining -= additional_bytes;
        } else {
            analysis.clear_non_workspace_semantic_indexes();
        }
    }

    for analysis in analyses {
        let allowed_bytes = workspace_floor.saturating_add(remaining);
        analysis.workspace_symbols.retain_wire_budget(allowed_bytes);
        let additional_bytes = analysis
            .workspace_symbol_wire_bytes()
            .saturating_sub(workspace_floor);
        remaining = remaining.saturating_sub(additional_bytes);
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

    /// Sizing probe, not a gate test: replays the engine's 128-file sweep over a real corpus so
    /// the retention ceilings can be sized from measurements instead of guesses. Run it with
    /// `KRUSTY_SYMBOL_INDEX_CORPUS=/path/to/project cargo test -p krusty-lsp \
    ///  workspace_index_sizing_probe -- --nocapture`; without the variable it does nothing.
    #[test]
    fn workspace_index_sizing_probe() {
        let Ok(root) = std::env::var("KRUSTY_SYMBOL_INDEX_CORPUS") else {
            return;
        };
        let mut paths = Vec::new();
        let mut pending = vec![std::path::PathBuf::from(&root)];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|e| e == "kt") {
                    paths.push(path);
                }
            }
        }
        paths.sort();
        let mut project = ProjectSymbolIndex::default();
        let mut chunk_incomplete = 0usize;
        for chunk in paths.chunks(128) {
            let sources: Vec<(String, String)> = chunk
                .iter()
                .filter_map(|path| {
                    let text = std::fs::read_to_string(path).ok()?;
                    Some((format!("file://{}", path.display()), text))
                })
                .collect();
            let segment = WorkspaceSymbolIndex::from_uri_sources(
                sources
                    .iter()
                    .map(|(uri, text)| (uri.as_str(), text.as_str())),
            );
            chunk_incomplete += usize::from(!segment.is_complete());
            let uris: Vec<String> = sources.into_iter().map(|(uri, _)| uri).collect();
            project.replace_files(&uris, segment);
        }
        let retained = project
            .layers()
            .iter()
            .map(|layer| layer.retained_wire_bytes())
            .sum::<usize>();
        eprintln!(
            "PROBE files={} entries={} segments={} accounted_bytes={} ({:.1} MiB) \
             complete={} incomplete_chunks={}",
            paths.len(),
            project.entry_count(),
            project.segment_count(),
            retained,
            retained as f64 / (1024.0 * 1024.0),
            project.is_complete(),
            chunk_incomplete,
        );
    }

    #[test]
    fn json_wire_counter_matches_json_serialization() {
        let value = json!({
            "text": "line\n\u{1f642}",
            "values": [0, u32::MAX],
        });

        assert_eq!(
            serialized_json_wire_bytes(&value).unwrap(),
            serde_json::to_string(&value).unwrap().len()
        );
    }

    fn indexed(sources: &[&str], uris: &[&str]) -> WorkspaceSymbolIndex {
        let analysis = analyze_standalone_source_set(sources);
        // from_source_set already establishes the search order.
        let mut index = WorkspaceSymbolIndex::from_source_set(sources, &analysis.files);
        index.assign_uris(uris);
        index
    }

    fn encoded_names(index: &WorkspaceSymbolIndex, query: &str) -> Vec<String> {
        index
            .encode(query)
            .into_iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn workspace_symbols_match_wildcard_queries() {
        let index = indexed(
            &["class KotlinParser\nclass Lexer\nfun parseAll(): Int = 1\n"],
            &["file:///W.kt"],
        );

        // Leading wildcard: no literal prefix to binary-search, so this exercises the scan path.
        let mut interior = encoded_names(&index, "*parse*");
        interior.sort();
        assert_eq!(interior, vec!["KotlinParser", "parseAll"]);

        // A literal prefix narrows through the sorted array before the glob verifies.
        assert_eq!(encoded_names(&index, "Kotlin*"), vec!["KotlinParser"]);

        // `?` spans exactly one character.
        assert_eq!(encoded_names(&index, "Lexe?"), vec!["Lexer"]);
        assert!(encoded_names(&index, "Lexe??").is_empty());
    }

    #[test]
    fn wildcard_transition_budget_is_shared_across_project_layers() {
        let first = WorkspaceSymbolIndex::from_disk_sources(&[(
            "file:///First.kt",
            "package demo\nclass RepeatedTarget\n",
        )]);
        let second = WorkspaceSymbolIndex::from_disk_sources(&[(
            "file:///Second.kt",
            "package demo\nclass RepeatedTarget\n",
        )]);
        let query = WorkspaceQuery::parse("*target");
        let mut probe_steps = usize::MAX;
        assert_eq!(
            matches_glob("repeatedtarget", &query.pattern, &mut probe_steps),
            Some(true)
        );
        let one_match_steps = usize::MAX - probe_steps;

        let mut remaining_steps = one_match_steps;
        let encoded = WorkspaceSymbolIndex::encode_layers_with_glob_steps(
            "*target",
            &[&first, &second],
            &HashSet::new(),
            &mut remaining_steps,
        );

        assert_eq!(encoded.len(), 1);
        assert_eq!(encoded[0]["location"]["uri"], "file:///First.kt");
    }

    #[test]
    fn workspace_symbols_match_package_qualified_queries() {
        let index = indexed(
            &[
                "package kotlin.collections\nfun listOf(): Int = 1\n",
                "package demo.app\nfun listOf(): Int = 2\n",
                "package demo.collectionsExtra\nfun listOf(): Int = 3\n",
                "package Δοκιμή.app\nfun findMe(): Int = 4\n",
            ],
            &[
                "file:///A.kt",
                "file:///B.kt",
                "file:///C.kt",
                "file:///D.kt",
            ],
        );

        // Unqualified finds every declaration and keeps the compact source spelling.
        assert_eq!(encoded_names(&index, "listOf").len(), 3);

        // Qualifying by package selects one. The response name preserves that qualification so
        // clients that re-filter server results do not discard it as a non-match.
        let qualified = index.encode("kotlin.collections.listOf");
        assert_eq!(qualified.len(), 1);
        assert_eq!(qualified[0]["name"], "kotlin.collections.listOf");
        assert_eq!(qualified[0]["containerName"], "kotlin.collections");
        let slashed = index.encode("demo/app/listOf");
        assert_eq!(slashed.len(), 1);
        assert_eq!(slashed[0]["name"], "demo/app/listOf");

        // A package that matches nothing yields nothing, rather than falling back to the bare name.
        assert!(index.encode("nosuch.pkg.listOf").is_empty());
        assert_eq!(
            encoded_names(&index, "collections.listOf"),
            vec!["kotlin.collections.listOf"],
            "a package suffix must match on a segment boundary, not inside another segment"
        );
        assert_eq!(
            encoded_names(&index, "δοκιμή.app.findMe"),
            vec!["Δοκιμή.app.findMe"],
            "qualified package matching must fold Unicode source identifiers, not ASCII bytes only"
        );
    }

    #[test]
    fn workspace_symbols_match_a_wrong_layout_query() {
        let index = indexed(&["fun parse(): Int = 1\n"], &["file:///L.kt"]);

        // `зфкыу` is `parse` typed on a Cyrillic layout; the Latin form still matches too.
        assert_eq!(encoded_names(&index, "зфкыу"), vec!["parse"]);
        assert_eq!(encoded_names(&index, "parse"), vec!["parse"]);
    }

    #[test]
    fn a_trailing_double_colon_is_stripped_before_matching() {
        let index = indexed(&["class SyntheticParser\n"], &["file:///E.kt"]);

        // Zed keeps only the text after the last `::` for its own filter, so a trailing marker
        // disables client filtering; the server must not try to match the marker itself.
        assert_eq!(encoded_names(&index, "*parser*::"), vec!["SyntheticParser"]);
    }

    #[test]
    fn workspace_query_parses_packages_wildcards_and_the_escape() {
        // Plain name.
        let plain = WorkspaceQuery::parse("Widget");
        assert_eq!(plain.pattern, "widget");
        assert_eq!(plain.package, None);
        assert!(!plain.globbed);

        // Qualified: the last segment is the symbol, the rest is the package. `/` normalises to `.`.
        let dotted = WorkspaceQuery::parse("kotlin.collections.listOf");
        assert_eq!(dotted.pattern, "listof");
        assert_eq!(dotted.package.as_deref(), Some("kotlin.collections"));
        assert_eq!(dotted.separator, Some('.'));
        let slashed = WorkspaceQuery::parse("kotlin/collections/listOf");
        assert_eq!(slashed.package.as_deref(), Some("kotlin.collections"));
        assert_eq!(slashed.separator, Some('/'));

        // A trailing `::` is the client-filter escape and is not part of the pattern.
        let escaped = WorkspaceQuery::parse("*Parse*::");
        assert_eq!(escaped.pattern, "*parse*");
        assert!(escaped.globbed);

        // A non-empty tail after `::` is a path-style query, not the escape, so it stays intact.
        assert_eq!(WorkspaceQuery::parse("foo::bar").pattern, "foo::bar");
    }

    #[test]
    fn workspace_query_reports_the_literal_prefix_before_a_wildcard() {
        assert_eq!(WorkspaceQuery::parse("Foo*Bar").literal_prefix(), "foo");
        assert_eq!(WorkspaceQuery::parse("Fo?o").literal_prefix(), "fo");
        // A leading wildcard leaves nothing to binary-search on.
        assert_eq!(WorkspaceQuery::parse("*Parse*").literal_prefix(), "");
        assert_eq!(WorkspaceQuery::parse("plain").literal_prefix(), "plain");
    }

    #[test]
    fn glob_matching_handles_stars_and_single_characters() {
        let matches = |text, pattern| {
            let mut remaining_steps = usize::MAX;
            matches_glob(text, pattern, &mut remaining_steps).expect("unlimited test budget")
        };
        assert!(matches("kotlinparser", "*parse*"));
        assert!(matches("parser", "parse?"));
        assert!(!matches("parse", "parse?"));
        assert!(matches("foobar", "foo*"));
        assert!(matches("foobar", "*bar"));
        assert!(matches("foobar", "f?o*r"));
        assert!(!matches("foobar", "*baz"));
        assert!(matches("anything", "*"));
        assert!(matches("exact", "exact"));
        assert!(!matches("exact", "exac"));
        // Backtracking: the first `*` must give ground so the trailing literal can land.
        assert!(matches("aaa", "a*a"));
        assert!(matches("abcabc", "*abc"));
        assert!(matches("λparser", "?parser"));
        assert!(!matches("λparser", "??parser"));

        let mut no_steps = 0;
        assert_eq!(
            matches_glob("anything", "*thing", &mut no_steps),
            None,
            "glob evaluation must stop at its deterministic work ceiling"
        );
    }

    #[test]
    fn wrong_keyboard_layout_maps_to_the_intended_latin_query() {
        // `зфкыу` is `parse` typed on a Cyrillic layout.
        assert_eq!(qwerty_from_cyrillic("зфкыу").as_deref(), Some("parse"));
        // Nothing to do for a query that is already Latin.
        assert_eq!(qwerty_from_cyrillic("parse"), None);
        assert_eq!(
            qwerty_from_cyrillic("demo.parse"),
            None,
            "ordinary punctuation is not evidence of a Cyrillic layout"
        );
        // Mixed input still translates the Cyrillic run.
        assert_eq!(qwerty_from_cyrillic("зфкыу2").as_deref(), Some("parse2"));
    }

    #[test]
    fn workspace_symbol_names_survive_without_the_source_text() {
        let source = "class DetachedMarker\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let mut index = WorkspaceSymbolIndex::from_source_set(&[source], &analysis.files);
        index.assign_uris(&["file:///Detached.kt"]);
        let names = index
            .encode("DetachedMarker")
            .into_iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["DetachedMarker"]);
    }

    #[test]
    fn workspace_symbols_match_camel_hump_initials() {
        let source = "class StructuredSourceFile\nfun drawrect(): Int = 1\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let mut index = WorkspaceSymbolIndex::from_source_set(&[source], &analysis.files);
        index.assign_uris(&["file:///Humps.kt"]);
        let names = index
            .encode("ssf")
            .into_iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["StructuredSourceFile"]);
    }

    #[test]
    fn workspace_symbols_match_acronym_camel_boundaries() {
        let source = "class APIResponseCache\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let mut index = WorkspaceSymbolIndex::from_source_set(&[source], &analysis.files);
        index.assign_uris(&["file:///Acronym.kt"]);

        assert_eq!(index.encode("arc")[0]["name"], "APIResponseCache");
        assert_eq!(index.encode("a").len(), 1);
    }

    #[test]
    fn workspace_symbols_match_unicode_and_ordered_subsequences() {
        let source =
            "class StructuredSourceFile\nclass ÄtherMarker\nfun preResolvedValue(): Int = 1\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let mut index = WorkspaceSymbolIndex::from_source_set(&[source], &analysis.files);
        index.assign_uris(&["file:///Search.kt"]);

        assert_eq!(index.encode("sf")[0]["name"], "StructuredSourceFile");
        assert_eq!(index.encode("äm")[0]["name"], "ÄtherMarker");
        assert_eq!(index.encode("rsv")[0]["name"], "preResolvedValue");
    }

    #[test]
    fn workspace_symbol_prefix_matches_lead_substring_matches() {
        let source = "fun preresolveAll(): Int = 1\nfun resolve(): Int = 2\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let mut index = WorkspaceSymbolIndex::from_source_set(&[source], &analysis.files);
        index.assign_uris(&["file:///Rank.kt"]);

        let names = index
            .encode("resolve")
            .into_iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["resolve", "preresolveAll"]);
    }

    #[test]
    fn workspace_symbols_preserve_unicode_lowercase_matching_for_interior_matches() {
        let source = "class PrefixÄtherMarker\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let mut index = WorkspaceSymbolIndex::from_source_set(&[source], &analysis.files);
        index.assign_uris(&["file:///Unicode.kt"]);

        assert_eq!(index.encode("äther")[0]["name"], "PrefixÄtherMarker");
    }

    #[test]
    fn workspace_symbols_keep_unicode_lowercase_equivalence_boundaries() {
        let source = "class StraßeMarker\nclass ΣigmaMarker\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let mut index = WorkspaceSymbolIndex::from_source_set(&[source], &analysis.files);
        index.assign_uris(&["file:///Unicode.kt"]);

        assert_eq!(index.encode("straße")[0]["name"], "StraßeMarker");
        assert!(index.encode("strasse").is_empty());
        assert_eq!(index.encode("σm")[0]["name"], "ΣigmaMarker");
        assert!(index.encode("ςm").is_empty());
    }

    #[test]
    fn workspace_symbol_merge_is_not_capped_at_a_fixed_entry_count() {
        const ENTRIES: usize = 40 * 1024;
        let entry = |line: usize| {
            let lo = (line * 7) as u32;
            [
                0u32,
                lo,
                lo + 6,
                line as u32,
                0,
                line as u32,
                6,
                5,
                0,
                0,
                0,
                lo,
                lo + 6,
            ]
        };
        let mut index = WorkspaceSymbolIndex {
            entries: (0..ENTRIES / 2).map(entry).collect(),
            packages: vec!["package".into()],
            by_name: Vec::new(),
            by_initials: Vec::new(),
            names: vec!["Needle".into()],
            files: vec!["file:///Merged.kt".into()],
            initials: Vec::new(),
            lowercase_names: Vec::new(),
            complete: true,
            omissions: WorkspaceIndexOmissions::default(),
        };
        let mut other = WorkspaceSymbolIndex {
            entries: (ENTRIES / 2..ENTRIES).map(entry).collect(),
            packages: vec!["package".into()],
            by_name: Vec::new(),
            by_initials: Vec::new(),
            names: vec!["Needle".into()],
            files: vec!["file:///Merged.kt".into()],
            initials: Vec::new(),
            lowercase_names: Vec::new(),
            complete: true,
            omissions: WorkspaceIndexOmissions::default(),
        };
        index.rebuild_search_order();
        other.rebuild_search_order();
        index.merge_from(other);

        assert_eq!(
            index.entries.len(),
            ENTRIES,
            "merging must not drop entries past a fixed ceiling"
        );
        assert!(index.is_complete());
    }

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
        let mut index = WorkspaceSymbolIndex::from_source_set(&[source], &analysis.files);
        index.assign_uris(&["file:///WorkspaceSymbols.kt"]);

        assert_eq!(
            index.encode("KrustyWorkspaceParityBox"),
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
        assert_eq!(index.encode("krustyworkspaceparitybox").len(), 1);
        assert_eq!(index.encode("KWPB").len(), 1);
        assert_eq!(
            index.encode("nestedNeedle"),
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
        assert_eq!(index.encode("krustyWorkspaceParityNeedle").len(), 1);
        assert_eq!(index.encode("krustyWorkspaceParityValue").len(), 1);
        assert_eq!(index.encode("when")[0]["name"], "when");
        assert_eq!(index.encode("Constructed").len(), 1);

        let default_source = "class DefaultPackageMarker\n";
        let default_analysis = analyze_standalone_source_set(&[default_source]);
        let mut default_index =
            WorkspaceSymbolIndex::from_source_set(&[default_source], &default_analysis.files);
        default_index.assign_uris(&["file:///Default.kt"]);
        let encoded = default_index.encode("DefaultPackageMarker");
        assert_eq!(encoded[0]["containerName"], "");
    }

    #[test]
    fn replacing_a_file_drops_what_the_index_held_for_it() {
        let mut index = WorkspaceSymbolIndex::from_disk_sources(&[
            ("file:///Kept.kt", "package demo\nclass KeptType\n"),
            ("file:///Edited.kt", "package demo\nclass OldType\n"),
        ]);

        index.replace_files(
            &["file:///Edited.kt".to_string()],
            WorkspaceSymbolIndex::from_disk_sources(&[(
                "file:///Edited.kt",
                "package demo\nclass NewType\n",
            )]),
        );

        assert_eq!(index.encode("KeptType").len(), 1);
        assert_eq!(index.encode("NewType").len(), 1);
        assert!(
            index.encode("OldType").is_empty(),
            "a re-indexed file must not keep its previous declarations"
        );
    }

    #[test]
    fn replacing_an_unreadable_file_removes_it_without_a_replacement() {
        let mut index = WorkspaceSymbolIndex::from_disk_sources(&[(
            "file:///Deleted.kt",
            "package demo\nclass DeletedType\n",
        )]);

        // The producer attempted the file and returned nothing: it is gone, not unchanged.
        index.replace_files(
            &["file:///Deleted.kt".to_string()],
            WorkspaceSymbolIndex::default(),
        );

        assert!(index.encode("DeletedType").is_empty());
    }

    fn project_chunk(chunk: usize, files: usize) -> (Vec<String>, WorkspaceSymbolIndex) {
        let sources: Vec<(String, String)> = (0..files)
            .map(|index| {
                let n = chunk * files + index;
                (
                    format!("file:///src/File{n}.kt"),
                    format!("package sample.pkg{n}\nclass TypeNumber{n} {{\n  fun member{n}(): Int = 1\n}}\n"),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = sources
            .iter()
            .map(|(uri, text)| (uri.as_str(), text.as_str()))
            .collect();
        let uris = sources.iter().map(|(uri, _)| uri.clone()).collect();
        (uris, WorkspaceSymbolIndex::from_disk_sources(&borrowed))
    }

    #[test]
    fn the_project_layer_keeps_every_chunk_in_a_logarithmic_number_of_segments() {
        let mut project = ProjectSymbolIndex::default();
        const CHUNKS: usize = 64;
        const FILES: usize = 8;
        for chunk in 0..CHUNKS {
            let (uris, segment) = project_chunk(chunk, FILES);
            project.replace_files(&uris, segment);
        }

        // A class and its member per file, none dropped.
        assert_eq!(project.entry_count(), CHUNKS * FILES * 2);
        // Segments merge only while their sizes are within a factor of two, so their count grows
        // with the logarithm of the corpus rather than with the number of chunks. Re-merging into
        // one index per chunk is what this structure exists to avoid.
        assert!(
            project.segment_count() <= 8,
            "segments grew to {} over {CHUNKS} chunks",
            project.segment_count()
        );

        let live = WorkspaceSymbolIndex::default();
        let layers = project.layers();
        assert_eq!(
            live.encode_over("TypeNumber0", &layers, &HashSet::new())[0]["name"],
            "TypeNumber0"
        );
        let last = CHUNKS * FILES - 1;
        assert_eq!(
            live.encode_over(&format!("TypeNumber{last}"), &layers, &HashSet::new())[0]["location"]
                ["uri"],
            format!("file:///src/File{last}.kt")
        );
    }

    #[test]
    fn the_project_budget_covers_unmerged_segments_as_one_layer() {
        let mut project = ProjectSymbolIndex::default();
        let (first_uris, first) = project_chunk(0, 8);
        let first_cost = first.retained_wire_bytes();
        let limit = first_cost.saturating_add(first_cost / 2);
        project.replace_files_within(&first_uris, first, limit);

        let (second_uris, second) = project_chunk(1, 8);
        project.replace_files_within(&second_uris, second, limit);

        let retained = project
            .segments
            .iter()
            .map(WorkspaceSymbolIndex::retained_wire_bytes)
            .sum::<usize>();
        assert!(retained <= limit, "retained {retained} bytes over {limit}");
        assert!(
            !project.is_complete(),
            "admitting only part of the second unmerged segment must be observable"
        );
        // The trim's cost is recorded, not just flagged: the client log reports how many
        // declarations the retention ceiling actually took.
        assert!(
            project.omissions().dropped_entries > 0,
            "a retention trim must record how many declarations it dropped"
        );
    }

    #[test]
    fn coalesce_time_merge_drops_reach_the_project_aggregate() {
        // Segments are drained of omissions at admission; a drop recorded DURING the coalesce
        // merge lands on the retained target segment, which is never read for provenance again.
        // The residue must be absorbed into the project aggregate or the client log falls back to
        // the vague no-cause clause. The drop itself is simulated by seeding the second segment's
        // entries against a budget the coalesce cannot honor.
        let mut project = ProjectSymbolIndex::default();
        let (first_uris, first) = project_chunk(0, 8);
        let first_cost = first.retained_wire_bytes();
        project.replace_files_within(&first_uris, first, usize::MAX);
        assert_eq!(project.omissions().dropped_entries, 0);
        // Seed the retained segment with merge-shaped residue: entries the next coalesce drops
        // land exactly here, and only the post-merge drain can surface them.
        project
            .segments
            .last_mut()
            .expect("one segment was just admitted")
            .omissions
            .dropped_entries += 3;

        // Admit a second chunk sized to coalesce with the first (within a factor of two).
        let (second_uris, second) = project_chunk(1, 8);
        project.replace_files_within(&second_uris, second, first_cost.saturating_mul(4));

        assert_eq!(
            project.omissions().dropped_entries,
            3,
            "declarations dropped during a coalesce merge must be visible on the aggregate"
        );
        // And the retained segments hold no stranded provenance the aggregate does not know.
        assert!(
            project
                .segments
                .iter()
                .all(|segment| segment.omissions.is_empty()),
            "segment-held omissions would never be read again"
        );
    }

    #[test]
    fn a_broad_query_is_bounded_by_the_response_symbol_cap() {
        let sources: Vec<(String, String)> = (0..MAX_WORKSPACE_SYMBOL_RESPONSE_SYMBOLS * 2)
            .map(|n| {
                (
                    format!("file:///Broad{n}.kt"),
                    format!("package demo\nclass BroadType{n}\n"),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = sources
            .iter()
            .map(|(uri, text)| (uri.as_str(), text.as_str()))
            .collect();
        let index = WorkspaceSymbolIndex::from_disk_sources(&borrowed);

        // Every one of these matches on the strongest rung, so the cap decides the count.
        let encoded = index.encode("BroadType");
        assert_eq!(encoded.len(), MAX_WORKSPACE_SYMBOL_RESPONSE_SYMBOLS);
        assert!(encoded
            .iter()
            .all(|symbol| symbol["location"]["range"]["start"]["line"].is_number()));
    }

    #[test]
    fn removing_a_file_keeps_the_remaining_rank_order() {
        let mut index = WorkspaceSymbolIndex::from_disk_sources(&[
            ("file:///A.kt", "package demo\nclass RankAlpha\n"),
            ("file:///B.kt", "package demo\nclass RankBeta\n"),
            ("file:///C.kt", "package demo\nclass RankGamma\n"),
        ]);
        let before = index
            .encode("rank")
            .into_iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(before, vec!["RankAlpha", "RankBeta", "RankGamma"]);

        // Removal renumbers the rank orders rather than re-sorting them; the survivors must keep
        // both their order and their identity.
        index.remove_files(&["file:///B.kt".to_string()]);

        let after = index
            .encode("rank")
            .into_iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(after, vec!["RankAlpha", "RankGamma"]);
        assert_eq!(index.encode("rg")[0]["name"], "RankGamma");
        assert_eq!(index.file_uris(), ["file:///A.kt", "file:///C.kt"]);
    }

    #[test]
    fn rank_crosses_layers_rung_by_rung() {
        let mut project = ProjectSymbolIndex::default();
        // Indexed first, so it ends up in the older, lower segment.
        project.replace_files(
            &["file:///Exact.kt".to_string()],
            WorkspaceSymbolIndex::from_disk_sources(&[(
                "file:///Exact.kt",
                "package demo\nclass Parser\n",
            )]),
        );
        project.replace_files(
            &["file:///Weak.kt".to_string()],
            WorkspaceSymbolIndex::from_disk_sources(&[(
                "file:///Weak.kt",
                "package demo\nclass PlaceholderAdapterServiceRenderer\n",
            )]),
        );

        let live = WorkspaceSymbolIndex::default();
        let names = live
            .encode_over("parser", &project.layers(), &HashSet::new())
            .into_iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();

        // The deliberately synthetic long name matches `parser` only as a subsequence on the
        // weakest rung, and sits in a newer segment. Ranking a whole layer at a time would put it
        // first.
        assert_eq!(names.first().map(String::as_str), Some("Parser"));
    }

    #[test]
    fn re_indexing_a_file_replaces_it_in_whichever_segment_holds_it() {
        let mut project = ProjectSymbolIndex::default();
        for chunk in 0..16 {
            let (uris, segment) = project_chunk(chunk, 8);
            project.replace_files(&uris, segment);
        }
        let stale_uri = "file:///src/File3.kt".to_string();

        project.replace_files(
            std::slice::from_ref(&stale_uri),
            WorkspaceSymbolIndex::from_disk_sources(&[(
                stale_uri.as_str(),
                "package sample.pkg3\nclass RewrittenType\n",
            )]),
        );

        let live = WorkspaceSymbolIndex::default();
        let layers = project.layers();
        assert_eq!(
            live.encode_over("RewrittenType", &layers, &HashSet::new())
                .len(),
            1
        );
        // `TypeNumber3` is a prefix of `TypeNumber30`, so ask the location, not the name: nothing
        // the rewritten file used to declare may survive in an older segment.
        assert!(
            live.encode_over("TypeNumber", &layers, &HashSet::new())
                .iter()
                .all(|symbol| symbol["location"]["uri"] != stale_uri.as_str()),
            "the declarations the file used to hold must not survive in an older segment"
        );
        // Its neighbours in the same original chunk are untouched.
        assert_eq!(
            live.encode_over("TypeNumber4", &layers, &HashSet::new())[0]["location"]["uri"],
            "file:///src/File4.kt"
        );
    }

    #[test]
    fn the_live_layer_shadows_the_project_layer_for_files_it_names() {
        let project = WorkspaceSymbolIndex::from_disk_sources(&[
            ("file:///Open.kt", "package demo\nclass SavedShape\n"),
            ("file:///Closed.kt", "package demo\nclass ClosedShape\n"),
        ]);
        let live_source = "package demo\nclass EditedShape\n";
        let analysis = analyze_standalone_source_set(&[live_source]);
        let mut live = WorkspaceSymbolIndex::from_source_set(&[live_source], &analysis.files);
        live.assign_uris(&["file:///Open.kt"]);

        let names = |query: &str| {
            live.encode_over(query, &[&project], &HashSet::new())
                .into_iter()
                .map(|symbol| symbol["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };

        // The buffer's current text wins over what its file says on disk...
        assert_eq!(names("EditedShape"), vec!["EditedShape"]);
        assert!(names("SavedShape").is_empty());
        // ...while every unopened file stays searchable.
        assert_eq!(names("ClosedShape"), vec!["ClosedShape"]);
    }

    #[test]
    fn parse_only_indexing_matches_the_analyzed_source_set() {
        let source = "package demo\n\
                      class DiskType {\n\
                      \u{20}\u{20}fun diskMember(): Int = 1\n\
                      }\n\
                      fun diskFunction(): Int = 2\n\
                      typealias DiskAlias = Int\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let mut analyzed = WorkspaceSymbolIndex::from_source_set(&[source], &analysis.files);
        analyzed.assign_uris(&["file:///Disk.kt"]);

        // Nobody opened this file, so it is only parsed. The declarations must be the same ones a
        // fully analyzed source set produces, or search results would depend on what is open.
        let indexed = WorkspaceSymbolIndex::from_disk_sources(&[("file:///Disk.kt", source)]);

        for query in ["DiskType", "diskMember", "diskFunction", "DiskAlias"] {
            assert_eq!(
                indexed.encode(query),
                analyzed.encode(query),
                "parse-only extraction diverged for {query}"
            );
        }
        assert!(indexed.is_complete());
    }

    #[test]
    fn parse_only_indexing_skips_files_past_the_per_file_cap() {
        let small = "package demo\nclass SmallDiskType\n";
        let large = format!(
            "package demo\nclass LargeDiskType\n// {}\n",
            "x".repeat(MAX_INDEXED_FILE_BYTES)
        );
        let indexed = WorkspaceSymbolIndex::from_disk_sources(&[
            ("file:///Small.kt", small),
            ("file:///Large.kt", large.as_str()),
        ]);

        assert_eq!(indexed.encode("SmallDiskType").len(), 1);
        assert!(
            indexed.encode("LargeDiskType").is_empty(),
            "an oversized file must be skipped before it is parsed"
        );
        assert!(!indexed.is_complete());
    }

    #[test]
    fn workspace_symbols_name_their_own_files_after_binding() {
        let first = "package demo\nclass BoundFirst\n";
        let second = "package demo\nclass BoundSecond\n";
        let analysis = analyze_standalone_source_set(&[first, second]);
        let mut index = WorkspaceSymbolIndex::from_source_set(&[first, second], &analysis.files);
        index.assign_uris(&["file:///First.kt", "file:///Second.kt"]);

        // Nothing outside the index holds these sources any more, and a location still resolves.
        assert_eq!(
            index.encode("BoundSecond")[0]["location"]["uri"],
            "file:///Second.kt"
        );
        assert_eq!(index.file_uris(), ["file:///First.kt", "file:///Second.kt"]);
        assert!(index.is_complete());
    }

    #[test]
    fn binding_drops_entries_no_uri_names_and_reports_incompleteness() {
        let source = "package demo\nclass Named\n";
        let other = "package demo\nclass Unnamed\n";
        let analysis = analyze_standalone_source_set(&[source, other]);
        let mut index = WorkspaceSymbolIndex::from_source_set(&[source, other], &analysis.files);
        index.assign_uris(&["file:///Named.kt"]);

        assert_eq!(index.encode("Named").len(), 1);
        assert!(index.encode("Unnamed").is_empty());
        assert!(!index.is_complete());
    }

    #[test]
    fn merging_bound_indexes_unions_their_file_tables() {
        let first_source = "package demo\nclass MergedFirst\n";
        let second_source = "package demo\nclass MergedSecond\n";
        let first_analysis = analyze_standalone_source_set(&[first_source]);
        let second_analysis = analyze_standalone_source_set(&[second_source]);
        let mut first =
            WorkspaceSymbolIndex::from_source_set(&[first_source], &first_analysis.files);
        let mut second =
            WorkspaceSymbolIndex::from_source_set(&[second_source], &second_analysis.files);
        // Both number their only file 0; the union has to keep them apart.
        first.assign_uris(&["file:///MergedFirst.kt"]);
        second.assign_uris(&["file:///MergedSecond.kt"]);
        first.merge_from(second);

        assert_eq!(
            first.encode("MergedFirst")[0]["location"]["uri"],
            "file:///MergedFirst.kt"
        );
        assert_eq!(
            first.encode("MergedSecond")[0]["location"]["uri"],
            "file:///MergedSecond.kt"
        );
        assert!(first.is_complete());
    }

    #[test]
    fn a_positional_index_refuses_a_bound_merge_before_uri_assignment() {
        let positional_source = "package demo\nclass PositionalType\n";
        let analysis = analyze_standalone_source_set(&[positional_source]);
        let mut positional =
            WorkspaceSymbolIndex::from_source_set(&[positional_source], &analysis.files);
        let bound = WorkspaceSymbolIndex::from_disk_sources(&[(
            "file:///Bound.kt",
            "package demo\nclass BoundType\n",
        )]);

        // Both indexes use entry[0] == 0, but one means source position zero and the other means
        // file-table id zero. Combining them before the positional side is bound must not make
        // either interpretation win accidentally.
        positional.merge_from(bound);
        positional.assign_uris(&["file:///Positional.kt"]);

        assert_eq!(
            positional.encode("PositionalType")[0]["location"]["uri"],
            "file:///Positional.kt"
        );
        assert!(positional.encode("BoundType").is_empty());
        assert!(!positional.is_complete());
    }

    #[test]
    fn assigning_uris_twice_keeps_the_original_bound_locations() {
        let mut index = WorkspaceSymbolIndex::from_disk_sources(&[(
            "file:///Original.kt",
            "package demo\nclass BoundOnce\n",
        )]);

        // A second assignment would read file id zero as source position zero. Production gate and
        // release profiles disable debug assertions, so correctness must not depend on one.
        index.assign_uris(&["file:///Wrong.kt"]);

        assert_eq!(
            index.encode("BoundOnce")[0]["location"]["uri"],
            "file:///Original.kt"
        );
        assert!(!index.is_complete());
    }

    #[test]
    fn workspace_symbol_merge_deduplicates_module_overlap_after_file_remapping() {
        let source = "package demo\nclass OverlapType\nfun sharedFunction() = 1\n";
        let first_analysis = analyze_standalone_source_set(&[source]);
        let second_analysis = analyze_standalone_source_set(&[source]);
        let mut first = WorkspaceSymbolIndex::from_source_set(&[source], &first_analysis.files);
        let mut second = WorkspaceSymbolIndex::from_source_set(&[source], &second_analysis.files);
        let uris = [
            "file:///unused.kt",
            "file:///unused2.kt",
            "file:///unused3.kt",
            "file:///Shared.kt",
        ];
        first.remap_files(&[(0, 3)], 4);
        second.remap_files(&[(0, 3)], 4);
        first.assign_uris(&uris);
        second.assign_uris(&uris);
        first.merge_from(second);

        assert_eq!(first.encode("OverlapType").len(), 1);
        assert_eq!(first.encode("sharedFunction").len(), 1);
        // Only the file an entry actually names is retained, so the merged index no longer depends
        // on the source set it was assembled from.
        assert_eq!(first.file_uris(), ["file:///Shared.kt"]);
    }

    #[test]
    fn workspace_symbol_merge_rebuilds_search_order() {
        let first_source = "fun pretargetResult(): Int = 1\n";
        let second_source = "fun targetResult(): Int = 2\nclass APIResponseCache\n";
        let first_analysis = analyze_standalone_source_set(&[first_source]);
        let second_analysis = analyze_standalone_source_set(&[second_source]);
        let mut first =
            WorkspaceSymbolIndex::from_source_set(&[first_source], &first_analysis.files);
        let mut second =
            WorkspaceSymbolIndex::from_source_set(&[second_source], &second_analysis.files);
        second.remap_files(&[(0, 1)], 2);
        let uris = ["file:///First.kt", "file:///Second.kt"];
        first.assign_uris(&uris);
        second.assign_uris(&uris);

        assert_eq!(first.encode("target")[0]["name"], "pretargetResult");
        first.merge_from(second);
        let names = first
            .encode("target")
            .into_iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["targetResult", "pretargetResult"]);
        assert_eq!(first.encode("arc")[0]["name"], "APIResponseCache");
    }

    #[test]
    fn workspace_symbol_search_order_reuses_interned_long_name_keys() {
        const ENTRIES: usize = 256;
        let name = format!("A{}Result", "x".repeat(64 * 1024));
        let mut index = WorkspaceSymbolIndex {
            entries: (0..ENTRIES)
                .map(|line| [0, 0, 1, line as u32, 0, line as u32, 1, 12, 0, 0, 0, 0, 1])
                .collect(),
            packages: vec![String::new()],
            by_name: Vec::new(),
            by_initials: Vec::new(),
            names: vec![name],
            files: vec!["file:///Long.kt".into()],
            initials: Vec::new(),
            lowercase_names: Vec::new(),
            complete: true,
            omissions: WorkspaceIndexOmissions::default(),
        };

        index.rebuild_search_order();

        assert_eq!(index.names.len(), 1);
        assert_eq!(index.prefix_matches("a").len(), ENTRIES);
        assert_eq!(index.initials_matches("ar").len(), ENTRIES);
    }

    #[test]
    fn workspace_symbol_deserialization_rebuilds_ranked_orders() {
        let source =
            "fun pretargetValue(): Int = 1\nfun targetValue(): Int = 2\nclass APIResultStore\n";
        let analysis = analyze_standalone_source_set(&[source]);
        let mut index = WorkspaceSymbolIndex::from_source_set(&[source], &analysis.files);
        index.assign_uris(&["file:///Ranked.kt"]);
        let mut wire = serde_json::to_value(index).unwrap();
        wire["by_name"] = json!([0]);
        wire["by_initials"] = json!([0]);

        let rebuilt = serde_json::from_value::<WorkspaceSymbolIndex>(wire).unwrap();
        let names = rebuilt
            .encode("target")
            .into_iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["targetValue", "pretargetValue"]);
        assert_eq!(rebuilt.encode("ars")[0]["name"], "APIResultStore");
        assert!(rebuilt.is_complete());
    }

    #[test]
    fn workspace_symbol_deserialization_drops_invalid_entries_and_detaches_invalid_parents() {
        let wire = json!({
            "entries": [
                [0, 0, 4, 0, 0, 0, 4, 5, 0, 0, 0, 0, 30],
                [0, 5, 10, 1, 0, 1, 5, 6, 1, 0, 1, 5, 10],
                [1, 0, 9, 0, 0, 0, 9, 5, 1, 0, 2, 0, 9],
                [0, 31, 38, 2, 0, 2, 7, 5, 1, 0, 3, 31, 38],
                [0, 11, 16, 3, 0, 3, 5, 6, 1, 0, 99, 11, 16],
                [0, 17, 23, 4, 0, 4, 6, 6, 5, 0, 4, 17, 23],
                [0, 40, 49, 5, 0, 5, 9, 5, 0, 0, 5, 42, 49]
            ],
            "packages": [""],
            "by_name": [],
            "by_initials": [],
            "names": ["Root", "Child", "CrossFile", "Outside", "Orphan", "Malformed"],
            "files": ["file:///Symbols.kt", "file:///Other.kt"],
            "complete": true
        });

        let index = serde_json::from_value::<WorkspaceSymbolIndex>(wire).unwrap();

        assert_eq!(index.entries.len(), 5);
        assert!(!index.is_complete());
        assert_eq!(index.encode("Root")[0]["name"], "Root");
        assert_eq!(index.encode("Child")[0]["containerName"], "Root");
        for name in ["CrossFile", "Outside", "Orphan"] {
            assert_eq!(index.encode(name)[0]["containerName"], "");
        }
        assert!(index.encode("Malformed").is_empty());
    }

    #[test]
    fn workspace_symbol_remap_drops_unavailable_files_and_marks_incomplete() {
        let mut index = WorkspaceSymbolIndex {
            entries: vec![
                [0, 0, 4, 0, 0, 0, 4, 5, 0, 0, 0, 0, 4],
                [1, 0, 7, 0, 0, 0, 7, 5, 0, 0, 1, 0, 7],
            ],
            packages: vec![String::new()],
            by_name: Vec::new(),
            by_initials: Vec::new(),
            names: vec!["Kept".into(), "Removed".into()],
            files: vec!["file:///Kept.kt".into(), "file:///Removed.kt".into()],
            initials: Vec::new(),
            lowercase_names: Vec::new(),
            complete: true,
            omissions: WorkspaceIndexOmissions::default(),
        };
        index.rebuild_search_order();

        index.remap_files(&[(1, u32::MAX)], 1);

        assert!(!index.is_complete());
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.encode("kept")[0]["name"], "Kept");
        assert!(index.encode("removed").is_empty());
    }

    #[test]
    fn workspace_symbol_snapshot_and_expanded_response_are_bounded() {
        let mut budget = WorkspaceSymbolBudget::new();
        let capacity = budget.remaining_entry_capacity();
        let mut entries = Vec::with_capacity(capacity);
        for line in 0..capacity {
            if !budget.reserve_merged_entry_in_file(
                "Needle",
                line == 0,
                "package",
                line == 0,
                "file:///Needle.kt",
                line == 0,
            ) {
                break;
            }
            let lo = (line * 7) as u32;
            entries.push([
                0,
                lo,
                lo + 6,
                line as u32,
                0,
                line as u32,
                6,
                5,
                0,
                0,
                0,
                lo,
                lo + 6,
            ]);
        }
        assert!(!budget.reserve_merged_entry("Needle", false, "package", false));
        let entry_count = entries.len();
        let mut index = WorkspaceSymbolIndex {
            entries,
            packages: vec!["package".into()],
            by_name: Vec::new(),
            by_initials: Vec::new(),
            names: vec!["Needle".into()],
            files: vec!["file:///Needle.kt".into()],
            initials: Vec::new(),
            lowercase_names: Vec::new(),
            complete: false,
            omissions: WorkspaceIndexOmissions::default(),
        };
        index.rebuild_search_order();
        let snapshot_bytes = serialized_json_wire_bytes(&index).unwrap();
        assert!(
            snapshot_bytes <= budget.wire_bytes
                && snapshot_bytes <= MAX_SOURCE_SET_WORKSPACE_SYMBOL_INDEX_WIRE_BYTES,
            "retained workspace-symbol index exceeded its wire budget"
        );
        assert!(!index.is_complete());
        let mut analysis = DocumentAnalysis::empty();
        analysis.workspace_symbols = index.clone();
        retain_analysis_wire_budget(
            std::slice::from_mut(&mut analysis),
            MAX_RETAINED_ANALYSIS_BYTES,
        );
        assert!(!analysis.workspace_symbols.entries.is_empty());
        assert!(!analysis.workspace_symbols.is_complete());
        let encoded = index.encode("needle");
        assert!(encoded.len() < entry_count);
        assert!(
            serialized_json_wire_bytes(&encoded).unwrap() <= MAX_WORKSPACE_SYMBOL_WIRE_BYTES,
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

    #[test]
    fn selected_inherited_library_member_does_not_navigate_to_a_source_namesake() {
        let classpath = krusty::toolchain::stdlib_classpath();
        if classpath.scan_types().is_empty() {
            return;
        }
        let source = concat!(
            "class Namesake {\n",
            "    fun toString(flag: Boolean): String = \"source\"\n",
            "}\n",
            "fun use(value: Namesake): String = value.toString()\n",
        );
        let analysis = document_analysis_for(source);
        let call = source.rfind("toString").unwrap() as u32;

        let source_targets = analysis.definitions.get(call).collect::<Vec<_>>();
        assert!(
            source_targets.is_empty(),
            "source targets: {source_targets:?}"
        );
        let selected = analysis
            .library_definitions
            .get(call)
            .expect("the selected inherited classpath member is terminal");
        assert_eq!(selected.member_name, "toString");
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
            serialized_json_wire_bytes(&Value::Array(bounded.encode(&source))).unwrap()
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

        analysis.remap_navigation_files(&[(7, 2)], 3);

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
    fn workspace_symbol_budget_does_not_clear_other_semantic_indexes() {
        let mut analysis = DocumentAnalysis::empty();
        analysis.definitions = DefinitionIndex::build(
            vec![DefinitionOccurrence {
                span: Span::new(0, 1),
                target: DefinitionTarget {
                    file: 0,
                    span: Span::new(2, 3),
                },
            }],
            usize::MAX,
        );
        analysis.workspace_symbols = WorkspaceSymbolIndex {
            entries: vec![[0, 0, 6, 0, 0, 0, 6, 12, 0, 0, 0, 0, 6]],
            packages: vec![String::new()],
            by_name: vec![0],
            by_initials: vec![0],
            names: vec!["Result".repeat(1024)],
            files: vec!["file:///Result.kt".into()],
            initials: Vec::new(),
            lowercase_names: Vec::new(),
            complete: false,
            omissions: WorkspaceIndexOmissions::default(),
        };
        let non_workspace_bytes = analysis.non_workspace_semantic_wire_bytes();
        let mut empty = DocumentAnalysis::empty();
        empty.workspace_symbols.clear_incomplete();
        let empty_workspace_bytes = empty.workspace_symbol_wire_bytes();
        let budget = ANALYSIS_RESPONSE_FIXED_WIRE_BYTES
            .saturating_add(non_workspace_bytes)
            .saturating_add(empty_workspace_bytes);

        retain_analysis_wire_budget(std::slice::from_mut(&mut analysis), budget);

        assert_eq!(analysis.definitions.entry_count(), 1);
        assert!(analysis.workspace_symbols.entries.is_empty());
        assert!(!analysis.workspace_symbols.is_complete());
        assert!(analysis.retained_wire_bytes() <= budget);
    }

    #[test]
    fn aggregate_budget_partially_retains_workspace_symbols() {
        let entry_count = 128usize;
        let mut index = WorkspaceSymbolIndex {
            entries: (0..entry_count)
                .map(|line| {
                    let lo = (line * 7) as u32;
                    [
                        0,
                        lo,
                        lo + 6,
                        line as u32,
                        0,
                        line as u32,
                        6,
                        5,
                        0,
                        0,
                        0,
                        lo,
                        lo + 6,
                    ]
                })
                .collect(),
            packages: vec![String::new()],
            by_name: Vec::new(),
            by_initials: Vec::new(),
            names: vec!["Needle".into()],
            files: vec!["file:///Merged.kt".into()],
            initials: Vec::new(),
            lowercase_names: Vec::new(),
            complete: true,
            omissions: WorkspaceIndexOmissions::default(),
        };
        index.rebuild_search_order();
        let mut analysis = DocumentAnalysis::empty();
        analysis.definitions = DefinitionIndex::build(
            vec![DefinitionOccurrence {
                span: Span::new(0, 1),
                target: DefinitionTarget {
                    file: 0,
                    span: Span::new(2, 3),
                },
            }],
            usize::MAX,
        );
        analysis.workspace_symbols = index;
        let mut empty = DocumentAnalysis::empty();
        empty.workspace_symbols.clear_incomplete();
        let budget = ANALYSIS_RESPONSE_FIXED_WIRE_BYTES
            + analysis.non_workspace_semantic_wire_bytes()
            + empty.workspace_symbol_wire_bytes()
            + 2048;

        retain_analysis_wire_budget(std::slice::from_mut(&mut analysis), budget);

        assert_eq!(analysis.definitions.entry_count(), 1);
        assert!(!analysis.workspace_symbols.entries.is_empty());
        assert!(analysis.workspace_symbols.entries.len() < entry_count);
        assert!(!analysis.workspace_symbols.is_complete());
        assert!(analysis.retained_wire_bytes() <= budget);
        let restored: WorkspaceSymbolIndex =
            serde_json::from_slice(&serde_json::to_vec(&analysis.workspace_symbols).unwrap())
                .unwrap();
        assert_eq!(
            restored.entries.len(),
            analysis.workspace_symbols.entries.len()
        );
        assert!(!restored.is_complete());
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
    fn enum_entry_call_navigates_to_its_selected_override() {
        let source = "enum class Mode {\n\
                      A {\n\
                        override fun text() = \"A\"\n\
                        fun use() = text()\n\
                      };\n\
                      abstract fun text(): String\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let occurrences = source
            .match_indices("text")
            .map(|(offset, _)| offset as u32)
            .collect::<Vec<_>>();
        let target = DefinitionTarget {
            file: 0,
            span: Span::new(occurrences[0], occurrences[0] + 4),
        };

        assert_eq!(
            analysis.definitions.get(occurrences[1]).collect::<Vec<_>>(),
            vec![target]
        );
    }

    #[test]
    fn source_super_property_navigates_to_the_selected_declaration() {
        let source = "open class Base { open val x: Int = 1 }\n\
                      class Child : Base() {\n\
                        override val x: Int = 2\n\
                        fun read(): Int = super.x\n\
                      }\n";
        let analysis = analyze_for_lsp(&[source]).pop().unwrap();
        let declaration = source.find("x:").unwrap() as u32;
        let access = source.rfind("x").unwrap() as u32;

        assert_eq!(
            analysis.definitions.get(access).collect::<Vec<_>>(),
            vec![DefinitionTarget {
                file: 0,
                span: Span::new(declaration, declaration + 1),
            }]
        );
        assert!(analysis.library_definitions.get(access).is_none());
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

    #[test]
    fn semantic_tokens_keep_same_named_source_classifiers_package_qualified() {
        // Editor classification must follow the same internal classifier identity as the compiler.
        // A global simple-name table would let the later class declaration overwrite the imported
        // enum's kind and entries in the usage file.
        let first = "package first\nenum class Marker { ONE }";
        let second = "package second\nclass Marker { fun other(): Int = 1 }";
        let usage = "package use\nimport first.Marker\nfun pick(): Marker = Marker.ONE";
        let sources = [first, second, usage];
        let analysis = analyze_standalone_source_set(&sources);
        let highlight_symbols =
            HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
        let index = SemanticTokenIndex::from_source_set_file_analysis(
            usage,
            &analysis.files[2],
            &analysis.symbols,
            &highlight_symbols,
        );
        let tokens = decoded_tokens(&index);
        let lines = usage.lines().collect::<Vec<_>>();

        assert!(tokens.contains(&(1, lines[1].find("Marker").unwrap() as u32, 6, 2, 0,)));
        assert!(tokens.contains(&(2, lines[2].rfind("ONE").unwrap() as u32, 3, 10, 4,)));
    }

    #[test]
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
