//! JSON-RPC/LSP session state and bounded stdio dispatch.
//!
//! This module lives in the separate `krusty-lsp` package, so the batch compiler neither links JSON
//! support nor retains server state. A session stores only the latest text and compact hover,
//! completion, navigation, and highlighting data for each open document; full compiler analysis is
//! dropped after every open/change notification.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::super::{
    CompletionIndex, DefinitionIndex, DocumentAnalysis, DocumentSymbolIndex, FoldingRangeIndex,
    HoverIndex, LibraryDefinitionIndex, MaterializedDefinition, SemanticTokenIndex,
    SemanticTokenRange, SignatureHelpIndex, MAX_RETAINED_ANALYSIS_BYTES, SEMANTIC_TOKEN_MODIFIERS,
    SEMANTIC_TOKEN_TYPES,
};
use crate::compiler_analysis::LibraryRef;
use crate::server::engine::{
    AnalysisBatch, AnalysisEngine, AnalysisJob, EngineBackend, EngineEvent, MaterializeJob,
    MaterializeResult,
};
use crate::server::status::StatusReporter;
use crate::uri::{file_uri_to_path, path_to_file_uri};
use crate::worker::{source_set_fits, MAX_SOURCE_SET_BYTES};
use krusty::diag::{Diagnostic, DiagnosticKind, Severity};

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_HEADER_BYTES: usize = 8 * 1024;
const INPUT_QUEUE_CAPACITY: usize = 4;
const MAX_INPUT_DISPATCHES_BEFORE_MAINTENANCE: usize = 32;
const MAX_OPEN_DOCUMENTS: usize = 256;
const MAX_OPEN_SOURCE_BYTES: usize = MAX_RETAINED_ANALYSIS_BYTES;
const MAX_CONTENT_CHANGES: usize = 256;
const MAX_CONTENT_CHANGE_SCAN_BYTES: usize = MAX_SOURCE_SET_BYTES * 3;
const MAX_CONTENT_CHANGE_EDIT_BYTES: usize = MAX_SOURCE_SET_BYTES * 3;
const MAX_CONTENT_CHANGE_UNDO_BYTES: usize = MAX_SOURCE_SET_BYTES;
const MAX_BATCH_MESSAGES: usize = 256;
const MAX_BATCH_VALUE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SOURCE_SET_DIAGNOSTIC_ENTRIES: usize = 32 * 1024;
const MAX_SOURCE_SET_DIAGNOSTIC_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_SOURCE_SET_DIAGNOSTIC_WIRE_BYTES: usize = 8 * 1024 * 1024;
const DIAGNOSTIC_WIRE_FIXED_BYTES: usize = 256;
const MAX_PENDING_DIAGNOSTIC_REQUEST_BYTES: usize = 256 * 1024;
const MAX_RENAME_IDENTIFIER_BYTES: usize = 1024;
const MAX_RENAME_SPELLINGS: usize = 8;
pub(super) const MAX_RENAME_WIRE_BYTES: usize = 8 * 1024 * 1024;
const RENAME_DOCUMENT_WIRE_FIXED_BYTES: usize = 128;
const RENAME_EDIT_WIRE_FIXED_BYTES: usize = 192;
const DIAGNOSTIC_WARNING_BIT: u32 = 1 << 31;
const DIAGNOSTIC_INSPECTION_BIT: u32 = 1 << 30;
const DIAGNOSTIC_MESSAGE_MASK: u32 = !(DIAGNOSTIC_WARNING_BIT | DIAGNOSTIC_INSPECTION_BIT);
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(150);
const MAX_BATCH_DURATION: Duration = Duration::from_millis(500);
const ANALYSIS_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(1);
const ANALYSIS_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const SERVER_VERSION: &str = match option_env!("KRUSTY_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Severity of a message the analysis backend asks the server to show the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMessageKind {
    Info,
    Warning,
    Error,
}

impl ProjectMessageKind {
    /// LSP `MessageType` code.
    fn message_type(self) -> i64 {
        match self {
            ProjectMessageKind::Error => 1,
            ProjectMessageKind::Warning => 2,
            ProjectMessageKind::Info => 3,
        }
    }
}

/// What a project refresh tells the session to do.
#[derive(Default)]
pub struct ProjectFeedback {
    /// Re-analyze every open document — the classpath or module layout changed.
    pub reanalyze: bool,
    /// A one-line status to surface with `window/showMessage`.
    pub message: Option<(ProjectMessageKind, String)>,
    /// Messages for the language-server log.
    pub logs: Vec<String>,
}

impl ProjectFeedback {
    fn into_messages(self) -> Vec<Value> {
        let mut messages: Vec<Value> = self.logs.into_iter().map(log_message).collect();
        if let Some((kind, text)) = self.message {
            messages.push(show_message(kind, text));
        }
        messages
    }
}

#[derive(Clone, Default)]
pub struct DocumentAdmission {
    grouped: bool,
    source_roots: Vec<(PathBuf, usize, usize)>,
    visible_modules: Vec<Vec<usize>>,
}

impl DocumentAdmission {
    pub fn for_snapshot(snapshot: &crate::project::model::SourceModuleGraph) -> Self {
        let model = snapshot.model();
        if matches!(
            model.kind,
            crate::project::ProviderKind::Explicit | crate::project::ProviderKind::None
        ) {
            return Self::default();
        }
        let source_roots = model
            .modules
            .iter()
            .enumerate()
            .flat_map(|(module_index, module)| {
                module.source_roots.iter().map(move |root| {
                    (
                        root.path.clone(),
                        root.path.components().count(),
                        module_index,
                    )
                })
            })
            .collect();
        let visible_modules = snapshot
            .iter()
            .enumerate()
            .map(|(module_index, relations)| {
                let mut visible = relations.visible();
                visible.push(module_index);
                visible.sort_unstable();
                visible.dedup();
                visible
            })
            .collect();
        Self {
            grouped: true,
            source_roots,
            visible_modules,
        }
    }

    #[cfg(test)]
    pub fn for_model(model: &crate::project::ProjectModel) -> Self {
        Self::for_snapshot(&model.clone().into_source_module_graph())
    }

    pub fn accepts(&self, documents: &[(&str, usize)]) -> bool {
        let global_bytes = documents
            .iter()
            .try_fold(0usize, |total, (_, length)| total.checked_add(*length));
        if global_bytes.is_none_or(|bytes| bytes > MAX_OPEN_SOURCE_BYTES) {
            return false;
        }
        if !self.grouped {
            return source_set_fits(documents.iter().map(|(_, length)| *length));
        }
        let mut assignments = Vec::with_capacity(documents.len());
        for &(uri, length) in documents {
            if length > MAX_SOURCE_SET_BYTES {
                return false;
            }
            let assignment = url::Url::parse(uri)
                .ok()
                .and_then(|uri| uri.to_file_path().ok())
                .and_then(|path| {
                    self.source_roots
                        .iter()
                        .filter(|(root, _, _)| path.starts_with(root))
                        .map(|(_, depth, module_index)| (*depth, *module_index))
                        .max()
                        .map(|(_, module_index)| module_index)
                });
            assignments.push(assignment);
        }
        let mut active_modules = assignments.iter().flatten().copied().collect::<Vec<_>>();
        active_modules.sort_unstable();
        active_modules.dedup();
        active_modules.into_iter().all(|module_index| {
            let Some(visible) = self.visible_modules.get(module_index) else {
                return false;
            };
            source_set_fits(documents.iter().zip(&assignments).filter_map(
                |((_, length), assignment)| {
                    assignment
                        .is_some_and(|assigned| visible.binary_search(&assigned).is_ok())
                        .then_some(*length)
                },
            ))
        })
    }
}

/// Analysis and project-model operations behind an LSP session.
pub trait Analysis {
    fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis>;

    fn document_admission(&self) -> DocumentAdmission {
        DocumentAdmission::default()
    }

    /// Analyze open documents together with project support sources.
    fn analyze_open_documents(
        &mut self,
        documents: &[(&str, &str)],
        _open_uris: &[&str],
    ) -> (Vec<DocumentAnalysis>, Vec<(String, String)>) {
        let sources = documents
            .iter()
            .map(|(_, source)| *source)
            .collect::<Vec<_>>();
        (self.analyze(&sources), Vec::new())
    }

    fn materialize_library_definition(
        &mut self,
        _reference: &LibraryRef,
    ) -> Option<MaterializedDefinition> {
        None
    }

    fn analysis_ready(&self) -> bool {
        true
    }

    /// Whether the latest analysis attempt is waiting on infrastructure.
    fn analysis_pending(&self) -> bool {
        false
    }

    /// Adopt the workspace root and report the initial project state.
    fn set_workspace_root(&mut self, _root: Option<PathBuf>) -> ProjectFeedback {
        ProjectFeedback::default()
    }

    /// Glob patterns whose changes should trigger a project refresh, registered with the client
    /// after `initialized`. Globs rather than fixed paths so a newly created build file is caught
    /// too.
    fn watched_globs(&mut self) -> Vec<String> {
        Vec::new()
    }

    fn note_project_change(&mut self) {}

    /// Return true when a watched change requires immediate source analysis.
    fn note_watched_file_change(&mut self, _uri: &str) -> bool {
        self.note_project_change();
        false
    }

    fn project_refresh_due_in(&self) -> Option<Duration> {
        None
    }

    fn refresh_project(&mut self) -> ProjectFeedback {
        ProjectFeedback::default()
    }
}

/// URI-aware analysis for open Kotlin and Java documents.
pub struct DocumentAnalyzer;

impl Analysis for DocumentAnalyzer {
    fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
        crate::analysis::analyze_for_lsp(sources)
    }

    fn analyze_open_documents(
        &mut self,
        documents: &[(&str, &str)],
        _open_uris: &[&str],
    ) -> (Vec<DocumentAnalysis>, Vec<(String, String)>) {
        (
            crate::analysis::analyze_documents_for_lsp(documents),
            Vec::new(),
        )
    }
}

impl<F> Analysis for F
where
    F: FnMut(&[&str]) -> Vec<DocumentAnalysis>,
{
    fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
        self(sources)
    }
}

pub trait AnalysisBackend {
    fn analysis_ready(&self) -> bool;
    fn accepts_document_set(&self, documents: &[(&str, usize)]) -> bool {
        DocumentAdmission::default().accepts(documents)
    }
    fn submit(&mut self, job: AnalysisJob) -> Option<AnalysisBatch>;
    fn materialize(&mut self, job: MaterializeJob) -> Option<MaterializeResult> {
        Some(MaterializeResult {
            token: job.token,
            definition: None,
        })
    }
    fn set_workspace_root(&mut self, root: Option<PathBuf>) -> Option<ProjectFeedback>;
    fn watched_globs(&mut self) -> Vec<String>;
    fn note_project_change(&mut self);
    fn note_watched_file_change(&mut self, uri: &str) -> bool;
    fn note_watched_file_changes(&mut self, uris: &[String]) -> bool {
        let mut source_changed = false;
        for uri in uris {
            source_changed |= self.note_watched_file_change(uri);
        }
        source_changed
    }
    fn project_refresh_due_in(&self) -> Option<Duration>;
    fn refresh_project(&mut self) -> Option<ProjectFeedback>;
    fn set_ready(&mut self, _ready: bool) {}
}

pub struct InlineBackend<A>(A);

impl<A: Analysis> InlineBackend<A> {
    pub fn new(analyze: A) -> Self {
        InlineBackend(analyze)
    }
}

impl<A: Analysis> AnalysisBackend for InlineBackend<A> {
    fn analysis_ready(&self) -> bool {
        self.0.analysis_ready()
    }

    fn accepts_document_set(&self, documents: &[(&str, usize)]) -> bool {
        self.0.document_admission().accepts(documents)
    }

    fn submit(&mut self, job: AnalysisJob) -> Option<AnalysisBatch> {
        debug_assert!(self.0.analysis_ready());
        let docs: Vec<(&str, &str)> = job
            .documents
            .iter()
            .map(|(u, t, _)| (u.as_str(), t.as_str()))
            .collect();
        let open: Vec<&str> = job.open_uris.iter().map(String::as_str).collect();
        let (analyses, support_documents) = self.0.analyze_open_documents(&docs, &open);
        Some(AnalysisBatch {
            analyzed: job
                .documents
                .iter()
                .map(|(u, _, v)| (u.clone(), *v))
                .collect(),
            analyses,
            support_documents,
            pending: self.0.analysis_pending(),
        })
    }

    fn materialize(&mut self, job: MaterializeJob) -> Option<MaterializeResult> {
        let definition = self.0.materialize_library_definition(&job.reference);
        Some(MaterializeResult {
            token: job.token,
            definition,
        })
    }

    fn set_workspace_root(&mut self, root: Option<PathBuf>) -> Option<ProjectFeedback> {
        Some(self.0.set_workspace_root(root))
    }

    fn watched_globs(&mut self) -> Vec<String> {
        self.0.watched_globs()
    }

    fn note_project_change(&mut self) {
        self.0.note_project_change()
    }

    fn note_watched_file_change(&mut self, uri: &str) -> bool {
        self.0.note_watched_file_change(uri)
    }

    fn project_refresh_due_in(&self) -> Option<Duration> {
        self.0.project_refresh_due_in()
    }

    fn refresh_project(&mut self) -> Option<ProjectFeedback> {
        Some(self.0.refresh_project())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Position {
    line: u32,
    character: u32,
}

/// Translate an LSP UTF-16 position into a source byte offset.
pub fn position_to_byte_offset(text: &str, target: Position) -> Option<u32> {
    let mut scan_budget = usize::MAX;
    position_to_byte_offset_with_budget(text, target, &mut scan_budget)
}

fn position_to_byte_offset_with_budget(
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

impl Position {
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
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

pub struct Dispatch {
    pub messages: Vec<Value>,
    pub exit: bool,
    pub exit_code: i32,
}

impl Dispatch {
    fn messages(messages: Vec<Value>) -> Self {
        Self {
            messages,
            exit: false,
            exit_code: 0,
        }
    }

    fn none() -> Self {
        Self::messages(Vec::new())
    }
}

/// `(start line, start UTF-16 column, end line, end UTF-16 column,
/// packed severity + interned message id)`.
type DiagnosticEntry = [u32; 5];
/// `(source lo, source hi, packed severity + interned message id)` while positions are resolved.
type PendingDiagnosticEntry = [u32; 3];

#[derive(Default)]
struct DiagnosticBudget {
    entries: usize,
    text_bytes: usize,
    wire_bytes: usize,
}

#[derive(Default)]
struct DiagnosticIndex {
    entries: Vec<DiagnosticEntry>,
    messages: Vec<String>,
}

impl DiagnosticIndex {
    fn from_diagnostics(
        diagnostics: Vec<Diagnostic>,
        text: &str,
        budget: &mut DiagnosticBudget,
    ) -> DiagnosticIndex {
        let mut pending = Vec::with_capacity(
            diagnostics
                .len()
                .min(256)
                .min(MAX_SOURCE_SET_DIAGNOSTIC_ENTRIES.saturating_sub(budget.entries)),
        );
        let mut message_ids = HashMap::<String, u32>::new();
        for diagnostic in diagnostics {
            let span = diagnostic.editor_span.unwrap_or(diagnostic.span);
            let message = lsp_diagnostic_message(diagnostic.msg);
            let existing_message_id = message_ids.get(&message).copied();
            let retained_bytes = if existing_message_id.is_none() {
                message.capacity()
            } else {
                0
            };
            let wire_bytes =
                DIAGNOSTIC_WIRE_FIXED_BYTES.saturating_add(json_string_wire_bytes(&message));
            if budget.entries >= MAX_SOURCE_SET_DIAGNOSTIC_ENTRIES
                || retained_bytes
                    > MAX_SOURCE_SET_DIAGNOSTIC_TEXT_BYTES.saturating_sub(budget.text_bytes)
                || wire_bytes
                    > MAX_SOURCE_SET_DIAGNOSTIC_WIRE_BYTES.saturating_sub(budget.wire_bytes)
            {
                break;
            }
            let message_id = if let Some(message_id) = existing_message_id {
                message_id
            } else {
                let Ok(message_id) = u32::try_from(message_ids.len()) else {
                    break;
                };
                if message_id > DIAGNOSTIC_MESSAGE_MASK {
                    break;
                }
                message_ids.insert(message, message_id);
                message_id
            };
            let severity = match diagnostic.severity {
                Severity::Error => 0,
                Severity::Warning => DIAGNOSTIC_WARNING_BIT,
            };
            let kind = if diagnostic.kind == DiagnosticKind::Inspection {
                DIAGNOSTIC_INSPECTION_BIT
            } else {
                0
            };
            pending.push([span.lo, span.hi.max(span.lo), severity | kind | message_id]);
            budget.entries += 1;
            budget.text_bytes += retained_bytes;
            budget.wire_bytes += wire_bytes;
        }

        let mut messages = vec![String::new(); message_ids.len()];
        for (message, message_id) in message_ids {
            messages[message_id as usize] = message;
        }
        let entries = resolve_diagnostic_positions(text, &pending);
        Self { entries, messages }
    }

    fn encode(&self) -> Vec<Value> {
        self.entries
            .iter()
            .map(|entry| {
                let message_id = entry[4] & DIAGNOSTIC_MESSAGE_MASK;
                let source = if entry[4] & DIAGNOSTIC_INSPECTION_BIT == 0 {
                    Value::String("Kotlin".to_string())
                } else {
                    Value::Null
                };
                json!({
                    "range": {
                        "start": {"line": entry[0], "character": entry[1]},
                        "end": {"line": entry[2], "character": entry[3]},
                    },
                    "severity": if entry[4] & DIAGNOSTIC_WARNING_BIT == 0 { 1 } else { 2 },
                    "source": source,
                    "message": self.messages[message_id as usize],
                })
            })
            .collect()
    }
}

fn json_string_wire_bytes(value: &str) -> usize {
    value.bytes().fold(2usize, |bytes, byte| {
        bytes.saturating_add(match byte {
            b'"' | b'\\' | b'\x08' | b'\t' | b'\n' | b'\x0c' | b'\r' => 2,
            0x00..=0x1f => 6,
            _ => 1,
        })
    })
}

fn resolve_diagnostic_positions(
    text: &str,
    pending: &[PendingDiagnosticEntry],
) -> Vec<DiagnosticEntry> {
    pending
        .iter()
        .zip(resolve_span_positions(
            text,
            pending.iter().map(|entry| (entry[0], entry[1])),
        ))
        .map(|(entry, position)| [position[0], position[1], position[2], position[3], entry[2]])
        .collect()
}

fn resolve_span_positions(
    text: &str,
    spans: impl ExactSizeIterator<Item = (u32, u32)>,
) -> Vec<[u32; 4]> {
    let mut positions = vec![[0u32; 4]; spans.len()];
    let mut endpoints = Vec::with_capacity(spans.len().saturating_mul(2));
    for (index, (lo, hi)) in spans.enumerate() {
        let slot = index.saturating_mul(2);
        endpoints.push((lo, slot));
        endpoints.push((hi, slot + 1));
    }
    endpoints.sort_unstable_by_key(|endpoint| endpoint.0);
    let mut characters = text.char_indices().peekable();
    let mut line = 0u32;
    let mut character = 0u32;
    let mut previous_was_cr = false;
    for (offset, slot) in endpoints {
        let limit = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(text.len());
        while let Some(&(byte, ch)) = characters.peek() {
            if byte >= limit || byte.saturating_add(ch.len_utf8()) > limit {
                break;
            }
            characters.next();
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
        let position = &mut positions[slot / 2];
        let coordinate = (slot % 2) * 2;
        position[coordinate] = line;
        position[coordinate + 1] = character;
    }
    positions
}

struct OpenDocument {
    text: String,
    version: i64,
    diagnostics: DiagnosticIndex,
    hover: HoverIndex,
    completion: CompletionIndex,
    signature_help: SignatureHelpIndex,
    semantic_tokens: SemanticTokenIndex,
    definitions: DefinitionIndex,
    type_definitions: DefinitionIndex,
    implementations: DefinitionIndex,
    library_definitions: LibraryDefinitionIndex,
    document_symbols: DocumentSymbolIndex,
    folding_ranges: FoldingRangeIndex,
    analysis_blocked: bool,
}

impl OpenDocument {
    fn clear_analysis(&mut self) {
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
        self.diagnostics = DiagnosticIndex::default();
    }
}

struct PendingDiagnosticRequest {
    id: Value,
    uri: String,
    retained_bytes: usize,
}

pub struct LspService<B> {
    documents: HashMap<String, OpenDocument>,
    source_set: Vec<(String, String)>,
    backend: B,
    analysis_dirty: bool,
    analysis_retry_at: Option<Instant>,
    analysis_retry_backoff: Duration,
    initialized: bool,
    client_initialized: bool,
    shutdown_requested: bool,
    pending_init_feedback: Option<ProjectFeedback>,
    pending_watched_globs: Vec<String>,
    analysis_in_flight: bool,
    resubmit_pending: bool,
    discard_in_flight: bool,
    pending_diagnostic_requests: VecDeque<PendingDiagnosticRequest>,
    pending_diagnostic_request_bytes: usize,
    next_materialize_token: u64,
    pending_materializations: HashMap<u64, Value>,
    status: StatusReporter,
}

impl<A: Analysis> LspService<InlineBackend<A>> {
    pub fn new(analyze: A) -> Self {
        Self::with_backend(InlineBackend::new(analyze))
    }
}

impl<B> LspService<B>
where
    B: AnalysisBackend,
{
    pub fn with_backend(backend: B) -> Self {
        Self {
            documents: HashMap::new(),
            source_set: Vec::new(),
            backend,
            analysis_dirty: false,
            analysis_retry_at: None,
            analysis_retry_backoff: Duration::ZERO,
            initialized: false,
            client_initialized: false,
            shutdown_requested: false,
            pending_init_feedback: None,
            pending_watched_globs: Vec::new(),
            analysis_in_flight: false,
            resubmit_pending: false,
            discard_in_flight: false,
            pending_diagnostic_requests: VecDeque::new(),
            pending_diagnostic_request_bytes: 0,
            next_materialize_token: 0,
            pending_materializations: HashMap::new(),
            status: StatusReporter::default(),
        }
    }

    pub fn open_document_count(&self) -> usize {
        self.documents.len()
    }

    fn analyzed_uris(&self) -> Vec<&str> {
        let mut uris = self
            .documents
            .iter()
            .filter(|(_, document)| !document.analysis_blocked)
            .map(|(uri, _)| uri.as_str())
            .collect::<Vec<_>>();
        uris.sort_unstable();
        uris
    }

    fn accepts_replacement(&self, uri: &str, text_len: usize) -> bool {
        if !self.documents.contains_key(uri) && self.documents.len() >= MAX_OPEN_DOCUMENTS {
            return false;
        }
        let documents = self
            .documents
            .iter()
            .filter_map(|(open_uri, document)| {
                (open_uri != uri).then_some((open_uri.as_str(), document.text.len()))
            })
            .chain(std::iter::once((uri, text_len)))
            .collect::<Vec<_>>();
        self.backend.accepts_document_set(&documents)
    }

    fn note_document_identity_change(&mut self) {
        if self.analysis_in_flight {
            self.discard_in_flight = true;
        }
    }

    fn take_analysis_job(&mut self) -> AnalysisJob {
        self.analysis_dirty = false;
        let documents = self
            .analyzed_uris()
            .into_iter()
            .map(|uri| {
                let open = &self.documents[uri];
                (uri.to_owned(), open.text.clone(), open.version)
            })
            .collect();
        let open_uris = self.documents.keys().cloned().collect();
        AnalysisJob {
            documents,
            open_uris,
        }
    }

    fn dispatch_pending_analysis(&mut self) -> Option<AnalysisJob> {
        if !self.analysis_dirty || !self.backend.analysis_ready() {
            return None;
        }
        if self.analysis_in_flight {
            self.resubmit_pending = true;
            return None;
        }
        let job = self.take_analysis_job();
        self.analysis_in_flight = true;
        Some(job)
    }

    fn apply_analysis_batch(&mut self, batch: AnalysisBatch) -> Vec<Value> {
        let stale = batch.analyzed.iter().any(|(uri, analyzed_version)| {
            self.documents
                .get(uri)
                .is_none_or(|open| open.version != *analyzed_version)
        });
        self.analysis_in_flight = false;
        let resubmit = std::mem::take(&mut self.resubmit_pending);
        if std::mem::take(&mut self.discard_in_flight) {
            self.analysis_dirty = true;
            return Vec::new();
        }
        if stale {
            self.analysis_dirty = true;
            return Vec::new();
        }
        let uris = batch
            .analyzed
            .iter()
            .map(|(uri, _)| uri.clone())
            .collect::<Vec<_>>();
        if batch.pending {
            self.schedule_analysis_retry(&uris);
            return Vec::new();
        }
        let mut diagnostic_budget = DiagnosticBudget::default();
        if batch.analyses.len() != batch.analyzed.len() {
            self.source_set.clear();
            for uri in &uris {
                let open = self
                    .documents
                    .get_mut(uri)
                    .expect("batch freshness checked before applying");
                open.clear_analysis();
                open.diagnostics = DiagnosticIndex::from_diagnostics(
                    vec![Diagnostic {
                        span: krusty::diag::Span::new(0, 0),
                        editor_span: None,
                        severity: Severity::Error,
                        kind: DiagnosticKind::Compiler,
                        msg: "analysis worker returned an incomplete source set".to_string(),
                        file: 0,
                    }],
                    &open.text,
                    &mut diagnostic_budget,
                );
            }
            if resubmit {
                self.analysis_dirty = true;
            }
            let mut messages = uris
                .into_iter()
                .map(|uri| {
                    let open = &self.documents[&uri];
                    publish_diagnostics(&uri, Some(open.version), &open.diagnostics)
                })
                .collect::<Vec<_>>();
            if !resubmit {
                messages.extend(self.complete_pending_diagnostic_requests());
            }
            return messages;
        }
        self.analysis_retry_at = None;
        self.analysis_retry_backoff = Duration::ZERO;
        let mut messages = Vec::with_capacity(batch.analyses.len());
        let mut analyzed_documents = Vec::with_capacity(batch.analyzed.len());
        for (analysis, (uri, _analyzed_version)) in batch.analyses.into_iter().zip(batch.analyzed) {
            let open = self
                .documents
                .get_mut(&uri)
                .expect("batch freshness checked before applying");
            open.hover = analysis.hover;
            open.completion = analysis.completion;
            open.signature_help = analysis.signature_help;
            open.semantic_tokens = analysis.semantic_tokens;
            open.definitions = analysis.definitions;
            open.type_definitions = analysis.type_definitions;
            open.implementations = analysis.implementations;
            open.library_definitions = analysis.library_definitions;
            open.document_symbols = analysis.document_symbols;
            open.folding_ranges = analysis.folding_ranges;
            open.diagnostics = DiagnosticIndex::from_diagnostics(
                analysis.diagnostics,
                &open.text,
                &mut diagnostic_budget,
            );
            messages.push(publish_diagnostics(
                &uri,
                Some(open.version),
                &open.diagnostics,
            ));
            analyzed_documents.push((uri, open.text.clone()));
        }
        self.source_set = analyzed_documents
            .into_iter()
            .chain(batch.support_documents)
            .collect();
        if resubmit {
            self.analysis_dirty = true;
        } else {
            messages.extend(self.complete_pending_diagnostic_requests());
        }
        messages
    }

    fn schedule_analysis_retry(&mut self, uris: &[String]) {
        self.source_set.clear();
        self.analysis_dirty = false;
        for uri in uris {
            if let Some(open) = self.documents.get_mut(uri) {
                open.clear_analysis();
            }
        }
        self.analysis_retry_backoff = if self.analysis_retry_backoff.is_zero() {
            ANALYSIS_RETRY_INITIAL_DELAY
        } else {
            self.analysis_retry_backoff
                .saturating_mul(2)
                .min(ANALYSIS_RETRY_MAX_DELAY)
        };
        self.analysis_retry_at = Some(Instant::now() + self.analysis_retry_backoff);
    }

    fn flush_analysis(&mut self) -> Vec<Value> {
        self.submit_pending_analysis()
    }

    pub fn handle(&mut self, message: Value) -> Dispatch {
        self.handle_inner(message, false)
    }

    fn handle_deferred(&mut self, message: Value) -> Dispatch {
        self.handle_inner(message, true)
    }

    fn handle_inner(&mut self, mut message: Value, defer_analysis: bool) -> Dispatch {
        let Some(object) = message.as_object_mut() else {
            return Dispatch::messages(vec![rpc_error(Value::Null, -32600, "invalid request")]);
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Dispatch::messages(vec![rpc_error(Value::Null, -32600, "invalid request")]);
        }

        let id = object.remove("id");
        let Some(method) = object
            .remove("method")
            .and_then(|method| method.as_str().map(str::to_owned))
        else {
            // A message with an id but no method is the client's response to a request the server
            // made (e.g. our `client/registerCapability`). There is nothing to reply to.
            if id.is_some() && (object.contains_key("result") || object.contains_key("error")) {
                return Dispatch::none();
            }
            return Dispatch::messages(vec![rpc_error(
                id.unwrap_or(Value::Null),
                -32600,
                "invalid request",
            )]);
        };
        if id.as_ref().is_some_and(|id| !is_request_id(id)) {
            return Dispatch::messages(vec![rpc_error(Value::Null, -32600, "invalid request")]);
        }
        let params = object.remove("params").unwrap_or(Value::Null);

        if method == "exit" {
            return Dispatch {
                messages: Vec::new(),
                exit: true,
                exit_code: if self.shutdown_requested { 0 } else { 1 },
            };
        }
        if self.shutdown_requested {
            return match id {
                Some(id) => {
                    Dispatch::messages(vec![rpc_error(id, -32600, "server has been shut down")])
                }
                None => Dispatch::none(),
            };
        }
        if !self.initialized && method != "initialize" {
            return match id {
                Some(id) => {
                    Dispatch::messages(vec![rpc_error(id, -32002, "server not initialized")])
                }
                None => Dispatch::none(),
            };
        }

        match method.as_str() {
            "initialize" => {
                let Some(id) = id else {
                    return Dispatch::none();
                };
                if self.initialized {
                    return Dispatch::messages(vec![rpc_error(
                        id,
                        -32600,
                        "server already initialized",
                    )]);
                }
                self.initialized = true;
                self.status
                    .set_supported(client_supports_work_done_progress(&params));
                self.pending_init_feedback =
                    self.backend.set_workspace_root(workspace_root(&params));
                Dispatch::messages(vec![rpc_result(
                    id,
                    json!({
                        "capabilities": {
                            "hoverProvider": true,
                            "definitionProvider": true,
                            "typeDefinitionProvider": true,
                            "implementationProvider": true,
                            "referencesProvider": true,
                            "renameProvider": true,
                            "documentSymbolProvider": true,
                            "foldingRangeProvider": true,
                            "diagnosticProvider": {
                                "interFileDependencies": true,
                                "workspaceDiagnostics": false,
                                "workDoneProgress": false,
                            },
                            "completionProvider": {
                                "resolveProvider": true,
                                "triggerCharacters": ["."],
                            },
                            "signatureHelpProvider": {
                                "triggerCharacters": ["(", ","],
                                "retriggerCharacters": [","],
                                "workDoneProgress": false,
                            },
                            "positionEncoding": "utf-16",
                            "semanticTokensProvider": {
                                "legend": {
                                    "tokenTypes": SEMANTIC_TOKEN_TYPES,
                                    "tokenModifiers": SEMANTIC_TOKEN_MODIFIERS,
                                },
                                "full": true,
                                "range": true,
                            },
                            "textDocumentSync": 2
                        },
                        "serverInfo": {
                            "name": "krusty-lsp",
                            "version": SERVER_VERSION
                        }
                    }),
                )])
            }
            "initialized" => {
                self.client_initialized = true;
                let mut messages = Vec::new();
                let mut globs = std::mem::take(&mut self.pending_watched_globs);
                globs.extend(self.backend.watched_globs());
                globs.sort_unstable();
                globs.dedup();
                if !globs.is_empty() {
                    messages.push(register_watched_files(&globs));
                }
                if let Some(feedback) = self.pending_init_feedback.take() {
                    messages.extend(feedback.into_messages());
                }
                Dispatch::messages(messages)
            }
            "workspace/didChangeWatchedFiles" => {
                self.did_change_watched_files(params, defer_analysis)
            }
            "$/cancelRequest" => match id {
                Some(id) => Dispatch::messages(vec![rpc_error(id, -32601, "method not found")]),
                None => self.cancel_request(params),
            },
            "textDocument/didOpen" => self.did_open(id, params, defer_analysis),
            "textDocument/didChange" => self.did_change(id, params, defer_analysis),
            "textDocument/didClose" => self.did_close(id, params, defer_analysis),
            "textDocument/hover" => self.hover(id, params),
            "textDocument/definition" => self.definition(id, params),
            "textDocument/typeDefinition" => self.type_definition(id, params),
            "textDocument/implementation" => self.implementation(id, params),
            "textDocument/references" => self.references(id, params),
            "textDocument/rename" => self.rename(id, params),
            "textDocument/documentSymbol" => self.document_symbols(id, params),
            "textDocument/foldingRange" => self.folding_ranges(id, params),
            "textDocument/diagnostic" => self.pull_diagnostics(id, params),
            "textDocument/completion" => self.completion(id, params),
            "textDocument/signatureHelp" => self.signature_help(id, params),
            "completionItem/resolve" => self.resolve_completion(id, params),
            "textDocument/semanticTokens/full" => self.semantic_tokens(id, params, false),
            "textDocument/semanticTokens/range" => self.semantic_tokens(id, params, true),
            "shutdown" => {
                let Some(id) = id else {
                    return Dispatch::none();
                };
                self.shutdown_requested = true;
                let mut messages = self.cancel_pending_diagnostic_requests(
                    false,
                    "diagnostic request was cancelled because the server is shutting down",
                );
                messages.extend(self.status.finish());
                messages.push(rpc_result(id, Value::Null));
                Dispatch::messages(messages)
            }
            _ => match id {
                Some(id) => Dispatch::messages(vec![rpc_error(id, -32601, "method not found")]),
                None => Dispatch::none(),
            },
        }
    }

    fn did_change_watched_files(&mut self, params: Value, defer_analysis: bool) -> Dispatch {
        let Ok(params) = serde_json::from_value::<DidChangeWatchedFilesParams>(params) else {
            return Dispatch::none();
        };
        let uris: Vec<String> = params
            .changes
            .into_iter()
            .map(|change| change.uri)
            .collect();
        let source_changed = self.backend.note_watched_file_changes(&uris);
        if !source_changed {
            return Dispatch::none();
        }
        self.analysis_dirty = true;
        if defer_analysis {
            Dispatch::none()
        } else {
            Dispatch::messages(self.flush_analysis())
        }
    }

    pub fn project_refresh_due_in(&self) -> Option<Duration> {
        let retry_due = self
            .analysis_retry_at
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        match (retry_due, self.backend.project_refresh_due_in()) {
            (Some(retry), Some(project)) => Some(retry.min(project)),
            (Some(retry), None) => Some(retry),
            (None, project) => project,
        }
    }

    pub fn run_due_project_refresh(&mut self) -> Vec<Value> {
        let mut messages = self.run_due_project_refresh_deferred();
        messages.extend(self.flush_analysis());
        messages
    }

    fn run_due_project_refresh_deferred(&mut self) -> Vec<Value> {
        if self
            .analysis_retry_at
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            self.analysis_retry_at = None;
            self.analysis_dirty = true;
        }
        let feedback = self.backend.refresh_project().unwrap_or_default();
        let reanalyze = feedback.reanalyze;
        let messages = feedback.into_messages();
        if reanalyze {
            self.analysis_dirty = true;
        }
        messages
    }

    #[cfg(test)]
    pub(crate) fn make_analysis_retry_due(&mut self) {
        if self.analysis_retry_at.is_some() {
            self.analysis_retry_at = Some(Instant::now());
        }
    }

    #[cfg(test)]
    fn force_initialized_for_test(&mut self) {
        self.initialized = true;
        self.client_initialized = true;
    }

    #[cfg(test)]
    fn open_document_for_test(&mut self, uri: &str, text: &str, version: i64) {
        self.documents.insert(
            uri.to_string(),
            OpenDocument {
                text: text.to_string(),
                version,
                diagnostics: DiagnosticIndex::default(),
                hover: HoverIndex::default(),
                completion: CompletionIndex::default(),
                signature_help: SignatureHelpIndex::default(),
                semantic_tokens: SemanticTokenIndex::default(),
                definitions: DefinitionIndex::default(),
                type_definitions: DefinitionIndex::default(),
                implementations: DefinitionIndex::default(),
                library_definitions: LibraryDefinitionIndex::default(),
                document_symbols: DocumentSymbolIndex::default(),
                folding_ranges: FoldingRangeIndex::default(),
                analysis_blocked: false,
            },
        );
    }

    #[cfg(test)]
    fn analysis_dirty_for_test(&self) -> bool {
        self.analysis_dirty
    }

    fn mark_analysis_dirty(&mut self) {
        self.analysis_dirty |= !self.documents.is_empty() || !self.source_set.is_empty();
    }

    fn client_initialized(&self) -> bool {
        self.client_initialized
    }

    fn defer_watched_globs(&mut self, globs: Vec<String>) {
        self.pending_watched_globs.extend(globs);
    }

    fn project_feedback_messages(&mut self, feedback: ProjectFeedback) -> Vec<Value> {
        if self.client_initialized {
            return feedback.into_messages();
        }
        let pending = self
            .pending_init_feedback
            .get_or_insert_with(ProjectFeedback::default);
        pending.reanalyze |= feedback.reanalyze;
        if feedback.message.is_some() {
            pending.message = feedback.message;
        }
        pending.logs.extend(feedback.logs);
        Vec::new()
    }

    fn set_backend_ready(&mut self, ready: bool) {
        self.backend.set_ready(ready);
    }

    fn submit_pending_analysis(&mut self) -> Vec<Value> {
        if self.shutdown_requested {
            return Vec::new();
        }
        let Some(job) = self.dispatch_pending_analysis() else {
            return Vec::new();
        };
        match self.backend.submit(job) {
            Some(batch) => self.apply_analysis_batch(batch),
            None => Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn mark_analysis_dirty_for_test(&mut self) {
        self.mark_analysis_dirty();
    }

    #[cfg(test)]
    fn resubmit_pending_for_test(&self) -> bool {
        self.resubmit_pending
    }

    #[cfg(test)]
    fn analysis_in_flight_for_test(&self) -> bool {
        self.analysis_in_flight
    }

    #[cfg(test)]
    fn document_diagnostic_count_for_test(&self, uri: &str) -> usize {
        self.documents
            .get(uri)
            .map_or(0, |open| open.diagnostics.entries.len())
    }

    fn did_open(&mut self, id: Option<Value>, params: Value, defer_analysis: bool) -> Dispatch {
        let Ok(params) = serde_json::from_value::<DidOpenParams>(params) else {
            return invalid_params(id);
        };
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        let replacing = self.documents.contains_key(&uri);
        let mut messages = if replacing {
            self.note_document_identity_change();
            self.cancel_pending_diagnostic_requests_for_uri(
                &uri,
                true,
                "diagnostic request was cancelled because the document changed",
            )
        } else {
            Vec::new()
        };
        if !self.accepts_replacement(&uri, params.text_document.text.len()) {
            let replaced_analyzed_document = self
                .documents
                .get(&uri)
                .is_some_and(|document| !document.analysis_blocked);
            if self.documents.contains_key(&uri) || self.documents.len() < MAX_OPEN_DOCUMENTS {
                self.documents.insert(
                    uri.clone(),
                    OpenDocument {
                        text: String::new(),
                        version,
                        diagnostics: analysis_limit_diagnostics(),
                        hover: HoverIndex::default(),
                        completion: CompletionIndex::default(),
                        signature_help: SignatureHelpIndex::default(),
                        semantic_tokens: SemanticTokenIndex::default(),
                        definitions: DefinitionIndex::default(),
                        type_definitions: DefinitionIndex::default(),
                        implementations: DefinitionIndex::default(),
                        library_definitions: LibraryDefinitionIndex::default(),
                        document_symbols: DocumentSymbolIndex::default(),
                        folding_ranges: FoldingRangeIndex::default(),
                        analysis_blocked: true,
                    },
                );
            }
            self.analysis_dirty |= replaced_analyzed_document;
            let fallback_diagnostics;
            let diagnostics = if let Some(open) = self.documents.get(&uri) {
                &open.diagnostics
            } else {
                fallback_diagnostics = analysis_limit_diagnostics();
                &fallback_diagnostics
            };
            messages.push(publish_diagnostics(&uri, Some(version), diagnostics));
            if !defer_analysis {
                messages.extend(self.flush_analysis());
            }
            return Dispatch::messages(messages);
        }
        self.documents.insert(
            uri.clone(),
            OpenDocument {
                text: params.text_document.text,
                version,
                diagnostics: DiagnosticIndex::default(),
                hover: HoverIndex::default(),
                completion: CompletionIndex::default(),
                signature_help: SignatureHelpIndex::default(),
                semantic_tokens: SemanticTokenIndex::default(),
                definitions: DefinitionIndex::default(),
                type_definitions: DefinitionIndex::default(),
                implementations: DefinitionIndex::default(),
                library_definitions: LibraryDefinitionIndex::default(),
                document_symbols: DocumentSymbolIndex::default(),
                folding_ranges: FoldingRangeIndex::default(),
                analysis_blocked: false,
            },
        );
        self.analysis_dirty = true;
        if !defer_analysis {
            messages.extend(self.flush_analysis());
        }
        Dispatch::messages(messages)
    }

    fn did_change(&mut self, id: Option<Value>, params: Value, defer_analysis: bool) -> Dispatch {
        let Ok(mut params) = serde_json::from_value::<DidChangeParams>(params) else {
            return invalid_params(id);
        };
        if params.content_changes.is_empty() || params.content_changes.len() > MAX_CONTENT_CHANGES {
            return invalid_params(id);
        }
        let uri = params.text_document.uri;
        let Some(open) = self.documents.get(&uri) else {
            return invalid_params(id);
        };
        if params.text_document.version <= open.version {
            return Dispatch::none();
        }
        if open.analysis_blocked {
            let Some(full_change) = params
                .content_changes
                .iter()
                .rposition(|change| change.range.is_none())
            else {
                return invalid_params(id);
            };
            params.content_changes.drain(..full_change);
        }
        let original = std::mem::take(&mut self.documents.get_mut(&uri).unwrap().text);
        let text = match apply_content_changes(original, params.content_changes) {
            Ok(text) => text,
            Err(original) => {
                self.documents.get_mut(&uri).unwrap().text = original;
                return invalid_params(id);
            }
        };
        let mut messages = self.cancel_pending_diagnostic_requests_for_uri(
            &uri,
            true,
            "diagnostic request was cancelled because the document changed",
        );
        if !self.accepts_replacement(&uri, text.len()) {
            let open = self.documents.get_mut(&uri).unwrap();
            let was_analyzed = !open.analysis_blocked;
            open.version = params.text_document.version;
            open.text.clear();
            open.clear_analysis();
            open.diagnostics = analysis_limit_diagnostics();
            open.analysis_blocked = true;
            self.analysis_dirty |= was_analyzed;
            messages.push(publish_diagnostics(
                &uri,
                Some(params.text_document.version),
                &open.diagnostics,
            ));
            if !defer_analysis {
                messages.extend(self.flush_analysis());
            }
            return Dispatch::messages(messages);
        }
        let open = self.documents.get_mut(&uri).unwrap();
        open.version = params.text_document.version;
        open.text = text;
        open.analysis_blocked = false;
        self.analysis_dirty = true;
        if !defer_analysis {
            messages.extend(self.flush_analysis());
        }
        Dispatch::messages(messages)
    }

    fn did_close(&mut self, id: Option<Value>, params: Value, defer_analysis: bool) -> Dispatch {
        let Ok(params) = serde_json::from_value::<DidCloseParams>(params) else {
            return invalid_params(id);
        };
        let uri = params.text_document.uri;
        if self.documents.remove(&uri).is_some() {
            self.note_document_identity_change();
        }
        self.analysis_dirty = true;
        let mut messages = if defer_analysis {
            Vec::new()
        } else {
            self.flush_analysis()
        };
        messages.push(publish_diagnostics(&uri, None, &DiagnosticIndex::default()));
        messages.extend(self.complete_pending_diagnostic_requests_for_uri(&uri));
        Dispatch::messages(messages)
    }

    fn hover(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<TextDocumentPositionParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let Some(offset) = position_to_byte_offset(&open.text, params.position) else {
            return invalid_params(Some(id));
        };
        let Some(hover) = open.hover.get(offset) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let contents = json!({
            "kind": "markdown",
            "value": format!("````kotlin\n{}\n````\n", hover.value),
        });
        Dispatch::messages(vec![rpc_result(
            id,
            json!({
                "contents": contents,
                "range": {
                    "start": byte_offset_to_position(&open.text, hover.span.lo as usize),
                    "end": byte_offset_to_position(&open.text, hover.span.hi as usize),
                }
            }),
        )])
    }

    fn document_symbols(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<DocumentSymbolParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        Dispatch::messages(vec![rpc_result(
            id,
            Value::Array(open.document_symbols.encode()),
        )])
    }

    fn folding_ranges(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<DocumentSymbolParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        Dispatch::messages(vec![rpc_result(
            id,
            Value::Array(open.folding_ranges.encode(&open.text)),
        )])
    }

    fn completion(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<TextDocumentPositionParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(
                id,
                json!({"isIncomplete": false, "items": []}),
            )]);
        };
        let Some(offset) = position_to_byte_offset(&open.text, params.position) else {
            return invalid_params(Some(id));
        };
        let is_incomplete =
            self.analysis_dirty || self.analysis_in_flight || !open.completion.is_complete();
        let items: Vec<_> = open
            .completion
            .complete(&open.text, offset)
            .into_iter()
            .enumerate()
            .map(|(rank, candidate)| {
                let mut label_details = serde_json::Map::new();
                if let Some(detail) = candidate.label_detail {
                    label_details.insert("detail".to_string(), Value::String(detail.to_string()));
                }
                if let Some(description) = candidate.label_description {
                    label_details.insert(
                        "description".to_string(),
                        Value::String(description.to_string()),
                    );
                }
                let mut item = json!({
                    "label": candidate.label,
                    "kind": candidate.kind,
                    "sortText": format!("{rank:010}"),
                });
                if !label_details.is_empty() {
                    item.as_object_mut()
                        .unwrap()
                        .insert("labelDetails".to_string(), Value::Object(label_details));
                }
                item
            })
            .collect();
        Dispatch::messages(vec![rpc_result(
            id,
            json!({"isIncomplete": is_incomplete, "items": items}),
        )])
    }

    fn signature_help(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<TextDocumentPositionParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let Some(offset) = position_to_byte_offset(&open.text, params.position) else {
            return invalid_params(Some(id));
        };
        Dispatch::messages(vec![rpc_result(
            id,
            open.signature_help.encode(offset).unwrap_or(Value::Null),
        )])
    }

    fn definition(&mut self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<TextDocumentPositionParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let Some(offset) = position_to_byte_offset(&open.text, params.position) else {
            return invalid_params(Some(id));
        };
        let locations = self.navigation_locations(&open.definitions, offset);
        let library_ref = locations
            .is_empty()
            .then(|| open.library_definitions.get(offset).cloned())
            .flatten();
        if !locations.is_empty() || library_ref.is_none() {
            return Dispatch::messages(vec![rpc_result(id, Value::Array(locations))]);
        }
        const MAX_PENDING_MATERIALIZATIONS: usize = 128;
        if self.pending_materializations.len() >= MAX_PENDING_MATERIALIZATIONS {
            return Dispatch::messages(vec![rpc_error(
                id,
                -32000,
                "too many pending dependency definitions",
            )]);
        }
        let token = self.next_materialize_token;
        self.next_materialize_token = self.next_materialize_token.wrapping_add(1);
        let result = self.backend.materialize(MaterializeJob {
            token,
            reference: library_ref.unwrap(),
        });
        match result {
            Some(result) => Dispatch::messages(vec![self.materialize_response(id, result)]),
            None => {
                self.pending_materializations.insert(token, id);
                Dispatch::none()
            }
        }
    }

    fn materialize_response(&self, id: Value, result: MaterializeResult) -> Value {
        let location = result.definition.and_then(|definition| {
            let uri = path_to_file_uri(&definition.path)?;
            Some(json!({
                "uri": uri,
                "range": {
                    "start": byte_offset_to_position(&definition.text, definition.lo as usize),
                    "end": byte_offset_to_position(&definition.text, definition.hi as usize),
                }
            }))
        });
        rpc_result(id, Value::Array(location.into_iter().collect()))
    }

    fn complete_materialization(&mut self, result: MaterializeResult) -> Option<Value> {
        let id = self.pending_materializations.remove(&result.token)?;
        Some(self.materialize_response(id, result))
    }

    fn type_definition(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<TextDocumentPositionParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let Some(offset) = position_to_byte_offset(&open.text, params.position) else {
            return invalid_params(Some(id));
        };
        let locations = self.navigation_locations(&open.type_definitions, offset);
        Dispatch::messages(vec![rpc_result(
            id,
            if locations.is_empty() {
                Value::Null
            } else {
                Value::Array(locations)
            },
        )])
    }

    fn implementation(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<TextDocumentPositionParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let Some(offset) = position_to_byte_offset(&open.text, params.position) else {
            return invalid_params(Some(id));
        };
        let locations = self.navigation_locations(&open.implementations, offset);
        Dispatch::messages(vec![rpc_result(
            id,
            if locations.is_empty() {
                Value::Null
            } else {
                Value::Array(locations)
            },
        )])
    }

    fn navigation_locations(&self, index: &DefinitionIndex, offset: u32) -> Vec<Value> {
        let targets = index.get(offset).collect::<Vec<_>>();
        if targets.is_empty() {
            return Vec::new();
        }
        targets
            .into_iter()
            .filter_map(|target| {
                let (uri, source) = self.source_set.get(target.file as usize)?;
                Some(json!({
                    "uri": uri,
                    "range": {
                        "start": byte_offset_to_position(
                            source,
                            target.span.lo as usize
                        ),
                        "end": byte_offset_to_position(
                            source,
                            target.span.hi as usize
                        ),
                    }
                }))
            })
            .collect()
    }

    fn references(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<ReferenceParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let Some(offset) = position_to_byte_offset(&open.text, params.position) else {
            return invalid_params(Some(id));
        };
        let targets = open.definitions.get(offset).collect::<Vec<_>>();
        if targets.is_empty() {
            return Dispatch::messages(vec![rpc_result(id, json!([]))]);
        }
        let target_ids = targets.into_iter().collect::<HashSet<_>>();
        let uris = self.analyzed_uris();

        let mut occurrences = Vec::new();
        if params.context.include_declaration {
            occurrences.extend(target_ids.iter().filter_map(|target| {
                let (uri, _) = self.source_set.get(target.file as usize)?;
                Some((uri.as_str(), target.span))
            }));
        }
        for (source_file, uri) in uris.iter().enumerate() {
            let document = &self.documents[*uri];
            occurrences.extend(
                document
                    .definitions
                    .occurrences_targeting(&target_ids)
                    .filter(|(span, target)| {
                        params.context.include_declaration
                            || source_file as u32 != target.file
                            || *span != target.span
                    })
                    .map(|(span, _)| (*uri, span)),
            );
        }
        occurrences.sort_unstable_by_key(|(uri, span)| (*uri, span.lo, span.hi));
        occurrences.dedup();
        let locations = occurrences
            .into_iter()
            .filter_map(|(uri, span)| {
                let source = self
                    .documents
                    .get(uri)
                    .map(|document| document.text.as_str())
                    .or_else(|| {
                        self.source_set.iter().find_map(|(source_uri, source)| {
                            (source_uri == uri).then_some(source.as_str())
                        })
                    })?;
                Some(json!({
                    "uri": uri,
                    "range": {
                        "start": byte_offset_to_position(source, span.lo as usize),
                        "end": byte_offset_to_position(source, span.hi as usize),
                    }
                }))
            })
            .collect::<Vec<_>>();
        Dispatch::messages(vec![rpc_result(id, Value::Array(locations))])
    }

    fn rename(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<RenameParams>(params) else {
            return invalid_params(Some(id));
        };
        if params.new_name.is_empty() || params.new_name.len() > MAX_RENAME_IDENTIFIER_BYTES {
            return invalid_params(Some(id));
        }
        let Some(open) = self.documents.get(&params.text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let Some(offset) = position_to_byte_offset(&open.text, params.position) else {
            return invalid_params(Some(id));
        };
        let targets = open.definitions.get(offset).collect::<HashSet<_>>();
        if targets.len() != 1 {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        }

        let analyzed_uris = self.analyzed_uris();
        if targets
            .iter()
            .any(|target| target.file as usize >= analyzed_uris.len())
        {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        }
        let mut response_uris = Vec::with_capacity(analyzed_uris.len());
        response_uris.push(params.text_document.uri.as_str());
        response_uris.extend(
            analyzed_uris
                .iter()
                .copied()
                .filter(|uri| *uri != params.text_document.uri),
        );

        let mut spellings = HashMap::<String, Vec<RenameTextChange>>::new();
        let mut wire_bytes = 64usize;
        let mut document_changes = Vec::new();
        for uri in response_uris {
            let Some(document) = self.documents.get(uri) else {
                continue;
            };
            let mut spans = document
                .definitions
                .occurrences_targeting(&targets)
                .map(|(span, _)| span)
                .collect::<Vec<_>>();
            spans.sort_unstable_by_key(|span| (span.lo, span.hi));
            spans.dedup();
            if spans.is_empty() {
                continue;
            }

            wire_bytes = wire_bytes
                .saturating_add(RENAME_DOCUMENT_WIRE_FIXED_BYTES)
                .saturating_add(json_string_wire_bytes(uri));
            if wire_bytes > MAX_RENAME_WIRE_BYTES {
                return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
            }
            let mut pending_edits = Vec::new();
            for span in spans {
                let Some(old_name) = document.text.get(span.lo as usize..span.hi as usize) else {
                    return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
                };
                if old_name.len() > MAX_RENAME_IDENTIFIER_BYTES {
                    return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
                }
                if !spellings.contains_key(old_name) {
                    if spellings.len() >= MAX_RENAME_SPELLINGS {
                        return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
                    }
                    let Some(changes) = rename_text_changes(old_name, &params.new_name) else {
                        return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
                    };
                    spellings.insert(old_name.to_string(), changes);
                }
                for change in &spellings[old_name] {
                    wire_bytes = wire_bytes
                        .saturating_add(RENAME_EDIT_WIRE_FIXED_BYTES)
                        .saturating_add(json_string_wire_bytes(&change.new_text));
                    if wire_bytes > MAX_RENAME_WIRE_BYTES {
                        return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
                    }
                    let (Ok(old_lo), Ok(old_hi)) =
                        (u32::try_from(change.old_lo), u32::try_from(change.old_hi))
                    else {
                        return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
                    };
                    let (Some(lo), Some(hi)) =
                        (span.lo.checked_add(old_lo), span.lo.checked_add(old_hi))
                    else {
                        return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
                    };
                    pending_edits.push(PendingRenameEdit {
                        lo,
                        hi,
                        new_text: change.new_text.clone(),
                    });
                }
            }
            let Some(edits) = encode_rename_edits(&document.text, pending_edits) else {
                return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
            };
            document_changes.push(json!({
                "textDocument": {
                    "uri": uri,
                    "version": document.version,
                },
                "edits": edits,
            }));
        }
        Dispatch::messages(vec![rpc_result(
            id,
            json!({"documentChanges": document_changes}),
        )])
    }

    fn resolve_completion(&self, id: Option<Value>, item: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        if !item.is_object() {
            return invalid_params(Some(id));
        }
        Dispatch::messages(vec![rpc_result(id, item)])
    }

    fn complete_pending_diagnostic_requests(&mut self) -> Vec<Value> {
        self.take_pending_diagnostic_requests_matching(|_| true)
            .into_iter()
            .map(|request| {
                diagnostic_report(
                    request.id,
                    self.documents
                        .get(&request.uri)
                        .map(|open| &open.diagnostics),
                )
            })
            .collect()
    }

    fn complete_pending_diagnostic_requests_for_uri(&mut self, uri: &str) -> Vec<Value> {
        self.take_pending_diagnostic_requests_matching(|request| request.uri == uri)
            .into_iter()
            .map(|request| {
                diagnostic_report(
                    request.id,
                    self.documents
                        .get(&request.uri)
                        .map(|open| &open.diagnostics),
                )
            })
            .collect()
    }

    fn cancel_pending_diagnostic_requests(
        &mut self,
        retrigger_request: bool,
        message: &str,
    ) -> Vec<Value> {
        self.take_pending_diagnostic_requests_matching(|_| true)
            .into_iter()
            .map(|request| diagnostic_server_cancelled(request.id, message, retrigger_request))
            .collect()
    }

    fn cancel_pending_diagnostic_requests_for_uri(
        &mut self,
        uri: &str,
        retrigger_request: bool,
        message: &str,
    ) -> Vec<Value> {
        self.take_pending_diagnostic_requests_matching(|request| request.uri == uri)
            .into_iter()
            .map(|request| diagnostic_server_cancelled(request.id, message, retrigger_request))
            .collect()
    }

    fn take_pending_diagnostic_requests_matching(
        &mut self,
        mut matches: impl FnMut(&PendingDiagnosticRequest) -> bool,
    ) -> Vec<PendingDiagnosticRequest> {
        let pending = std::mem::take(&mut self.pending_diagnostic_requests);
        let mut retained = VecDeque::with_capacity(pending.len());
        let mut matched = Vec::new();
        for request in pending {
            if matches(&request) {
                self.pending_diagnostic_request_bytes = self
                    .pending_diagnostic_request_bytes
                    .saturating_sub(request.retained_bytes);
                matched.push(request);
            } else {
                retained.push_back(request);
            }
        }
        self.pending_diagnostic_requests = retained;
        matched
    }

    fn cancel_request(&mut self, params: Value) -> Dispatch {
        let Some(id) = params.get("id").filter(|id| is_request_id(id)) else {
            return Dispatch::none();
        };
        let messages = self
            .take_pending_diagnostic_requests_matching(|request| request.id.eq(id))
            .into_iter()
            .map(|request| rpc_error(request.id, -32800, "request cancelled"))
            .collect();
        Dispatch::messages(messages)
    }

    fn pull_diagnostics(&mut self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<DocumentDiagnosticParams>(params) else {
            return invalid_params(Some(id));
        };
        let uri = params.text_document.uri;
        let waits_for_analysis = self
            .documents
            .get(&uri)
            .is_some_and(|open| !open.analysis_blocked)
            && (self.analysis_dirty
                || self.analysis_in_flight
                || self.resubmit_pending
                || self.analysis_retry_at.is_some());
        if !waits_for_analysis {
            return Dispatch::messages(vec![diagnostic_report(
                id,
                self.documents.get(&uri).map(|open| &open.diagnostics),
            )]);
        }

        let retained_bytes = retained_value_bytes(&id)
            .saturating_add(std::mem::size_of::<PendingDiagnosticRequest>())
            .saturating_add(uri.capacity());
        let replaced = self
            .pending_diagnostic_requests
            .iter()
            .find(|request| request.uri == uri);
        let current_bytes = self.pending_diagnostic_request_bytes.saturating_sub(
            replaced
                .map(|request| request.retained_bytes)
                .unwrap_or_default(),
        );
        if retained_bytes > MAX_PENDING_DIAGNOSTIC_REQUEST_BYTES.saturating_sub(current_bytes) {
            return Dispatch::messages(vec![diagnostic_server_cancelled(
                id,
                "diagnostic analysis request queue is full",
                true,
            )]);
        }
        let messages = self.cancel_pending_diagnostic_requests_for_uri(
            &uri,
            false,
            "diagnostic request was superseded by a newer pull",
        );
        self.pending_diagnostic_request_bytes = self
            .pending_diagnostic_request_bytes
            .saturating_add(retained_bytes);
        self.pending_diagnostic_requests
            .push_back(PendingDiagnosticRequest {
                id,
                uri,
                retained_bytes,
            });
        Dispatch::messages(messages)
    }

    fn semantic_tokens(&self, id: Option<Value>, params: Value, range: bool) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let parsed = if range {
            serde_json::from_value::<SemanticTokensRangeParams>(params)
                .map(|params| (params.text_document, Some(params.range)))
        } else {
            serde_json::from_value::<SemanticTokensParams>(params)
                .map(|params| (params.text_document, None))
        };
        let Ok((text_document, range)) = parsed else {
            return invalid_params(Some(id));
        };
        let Some(open) = self.documents.get(&text_document.uri) else {
            return Dispatch::messages(vec![rpc_result(id, Value::Null)]);
        };
        let range = range.map(|range| SemanticTokenRange {
            start_line: range.start.line,
            start_character: range.start.character,
            end_line: range.end.line,
            end_character: range.end.character,
        });
        Dispatch::messages(vec![rpc_result(
            id,
            json!({"data": open.semantic_tokens.encode(range)}),
        )])
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextDocumentItem {
    uri: String,
    version: i64,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DidOpenParams {
    text_document: TextDocumentItem,
}

#[derive(Deserialize)]
struct VersionedTextDocumentIdentifier {
    uri: String,
    version: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentChange {
    text: String,
    #[serde(default)]
    range: Option<Range>,
    #[serde(default)]
    range_length: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DidChangeParams {
    text_document: VersionedTextDocumentIdentifier,
    content_changes: Vec<ContentChange>,
}

#[derive(Deserialize)]
struct DidChangeWatchedFilesParams {
    changes: Vec<WatchedFileChange>,
}

#[derive(Deserialize)]
struct WatchedFileChange {
    uri: String,
}

#[derive(Deserialize)]
struct TextDocumentIdentifier {
    uri: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentSymbolParams {
    text_document: TextDocumentIdentifier,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentDiagnosticParams {
    text_document: TextDocumentIdentifier,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DidCloseParams {
    text_document: TextDocumentIdentifier,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextDocumentPositionParams {
    text_document: TextDocumentIdentifier,
    position: Position,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReferenceParams {
    text_document: TextDocumentIdentifier,
    position: Position,
    context: ReferenceContext,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameParams {
    text_document: TextDocumentIdentifier,
    position: Position,
    new_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReferenceContext {
    include_declaration: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct RenameTextChange {
    old_lo: usize,
    old_hi: usize,
    new_text: String,
}

struct PendingRenameEdit {
    lo: u32,
    hi: u32,
    new_text: String,
}

fn rename_text_changes(old: &str, new: &str) -> Option<Vec<RenameTextChange>> {
    let old_chars = old.chars().collect::<Vec<_>>();
    let new_chars = new.chars().collect::<Vec<_>>();
    if old_chars.len() > MAX_RENAME_IDENTIFIER_BYTES
        || new_chars.len() > MAX_RENAME_IDENTIFIER_BYTES
    {
        return None;
    }
    let columns = new_chars.len().checked_add(1)?;
    let cells = old_chars.len().checked_add(1)?.checked_mul(columns)?;
    let mut lcs = vec![0u16; cells];
    let cell = |old_index: usize, new_index: usize| old_index * columns + new_index;
    for old_index in (0..old_chars.len()).rev() {
        for new_index in (0..new_chars.len()).rev() {
            lcs[cell(old_index, new_index)] = if old_chars[old_index] == new_chars[new_index] {
                lcs[cell(old_index + 1, new_index + 1)].saturating_add(1)
            } else {
                lcs[cell(old_index + 1, new_index)].max(lcs[cell(old_index, new_index + 1)])
            };
        }
    }

    let old_offsets = char_offsets(old);
    let new_offsets = char_offsets(new);
    let mut changes = Vec::new();
    let mut old_index = 0usize;
    let mut new_index = 0usize;
    let mut pending = None::<(usize, usize)>;
    while old_index < old_chars.len() || new_index < new_chars.len() {
        if old_index < old_chars.len()
            && new_index < new_chars.len()
            && old_chars[old_index] == new_chars[new_index]
        {
            if let Some((old_start, new_start)) = pending.take() {
                changes.push(RenameTextChange {
                    old_lo: old_offsets[old_start],
                    old_hi: old_offsets[old_index],
                    new_text: new[new_offsets[new_start]..new_offsets[new_index]].to_string(),
                });
            }
            old_index += 1;
            new_index += 1;
        } else if new_index < new_chars.len()
            && (old_index == old_chars.len()
                || lcs[cell(old_index, new_index + 1)] > lcs[cell(old_index + 1, new_index)])
        {
            pending.get_or_insert((old_index, new_index));
            new_index += 1;
        } else {
            pending.get_or_insert((old_index, new_index));
            old_index += 1;
        }
    }
    if let Some((old_start, new_start)) = pending {
        changes.push(RenameTextChange {
            old_lo: old_offsets[old_start],
            old_hi: old_offsets[old_index],
            new_text: new[new_offsets[new_start]..new_offsets[new_index]].to_string(),
        });
    }
    Some(changes)
}

fn char_offsets(value: &str) -> Vec<usize> {
    value
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(value.len()))
        .collect()
}

fn encode_rename_edits(text: &str, pending: Vec<PendingRenameEdit>) -> Option<Vec<Value>> {
    for edit in &pending {
        let (Ok(lo), Ok(hi)) = (usize::try_from(edit.lo), usize::try_from(edit.hi)) else {
            return None;
        };
        if hi < lo || hi > text.len() || !text.is_char_boundary(lo) || !text.is_char_boundary(hi) {
            return None;
        }
    }
    let positions = resolve_span_positions(text, pending.iter().map(|edit| (edit.lo, edit.hi)));

    Some(
        pending
            .into_iter()
            .zip(positions)
            .map(|(edit, position)| {
                json!({
                    "range": {
                        "start": {"line": position[0], "character": position[1]},
                        "end": {"line": position[2], "character": position[3]},
                    },
                    "newText": edit.new_text,
                })
            })
            .collect(),
    )
}

#[derive(Clone, Copy, Deserialize)]
struct Range {
    start: Position,
    end: Position,
}

enum ChangeUndo {
    Range {
        start: usize,
        inserted_len: usize,
        original: String,
    },
    Full(String),
}

struct ContentChangeBudget {
    scan_bytes: usize,
    edit_bytes: usize,
    undo_bytes: usize,
}

impl ContentChangeBudget {
    fn new() -> Self {
        Self {
            scan_bytes: MAX_CONTENT_CHANGE_SCAN_BYTES,
            edit_bytes: MAX_CONTENT_CHANGE_EDIT_BYTES,
            undo_bytes: MAX_CONTENT_CHANGE_UNDO_BYTES,
        }
    }

    fn charge_edit(&mut self, bytes: usize) -> Option<()> {
        self.edit_bytes = self.edit_bytes.checked_sub(bytes)?;
        Some(())
    }

    fn charge_undo(&mut self, bytes: usize) -> Option<()> {
        self.undo_bytes = self.undo_bytes.checked_sub(bytes)?;
        Some(())
    }

    fn reset_undo(&mut self) {
        self.undo_bytes = MAX_CONTENT_CHANGE_UNDO_BYTES;
    }
}

fn apply_content_changes(text: String, changes: Vec<ContentChange>) -> Result<String, String> {
    apply_content_changes_with_budget(text, changes, ContentChangeBudget::new())
}

fn apply_content_changes_with_budget(
    mut text: String,
    changes: Vec<ContentChange>,
    mut budget: ContentChangeBudget,
) -> Result<String, String> {
    let mut undo = Vec::with_capacity(changes.len());
    for change in changes {
        if change.range.is_none() && !undo.is_empty() {
            rollback_content_changes(&mut text, std::mem::take(&mut undo));
            budget.reset_undo();
        }
        let Some(change) = apply_content_change(&mut text, change, &mut budget) else {
            rollback_content_changes(&mut text, undo);
            return Err(text);
        };
        undo.push(change);
    }
    Ok(text)
}

fn apply_content_change(
    text: &mut String,
    change: ContentChange,
    budget: &mut ContentChangeBudget,
) -> Option<ChangeUndo> {
    if let Some(range) = change.range {
        let (start, end) =
            content_change_range(text, range, change.range_length, &mut budget.scan_bytes)?;
        let removed_len = end - start;
        let next_len = text
            .len()
            .checked_sub(removed_len)?
            .checked_add(change.text.len())?;
        if next_len > MAX_SOURCE_SET_BYTES {
            return None;
        }
        let shifted_len = if removed_len == change.text.len() {
            0
        } else {
            text.len() - end
        };
        let edit_bytes = removed_len
            .checked_add(change.text.len())?
            .checked_add(shifted_len)?;
        budget.charge_edit(edit_bytes)?;
        budget.charge_undo(removed_len)?;
        let original = text[start..end].to_string();
        let inserted_len = change.text.len();
        text.replace_range(start..end, &change.text);
        Some(ChangeUndo::Range {
            start,
            inserted_len,
            original,
        })
    } else {
        if change.text.len() > MAX_SOURCE_SET_BYTES {
            return None;
        }
        budget.charge_undo(text.len())?;
        Some(ChangeUndo::Full(std::mem::replace(text, change.text)))
    }
}

fn content_change_range(
    text: &str,
    range: Range,
    expected_utf16_len: Option<u32>,
    scan_budget: &mut usize,
) -> Option<(usize, usize)> {
    let start = position_to_byte_offset_with_budget(text, range.start, scan_budget)? as usize;
    let end = position_to_byte_offset_with_budget(text, range.end, scan_budget)? as usize;
    if start > end {
        return None;
    }
    if let Some(expected) = expected_utf16_len {
        let mut actual = 0usize;
        for ch in text[start..end].chars() {
            *scan_budget = scan_budget.checked_sub(ch.len_utf8())?;
            actual = actual.checked_add(ch.len_utf16())?;
        }
        if actual != expected as usize {
            return None;
        }
    }
    Some((start, end))
}

fn rollback_content_changes(text: &mut String, undo: Vec<ChangeUndo>) {
    for change in undo.into_iter().rev() {
        match change {
            ChangeUndo::Range {
                start,
                inserted_len,
                original,
            } => text.replace_range(start..start + inserted_len, &original),
            ChangeUndo::Full(previous) => *text = previous,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SemanticTokensParams {
    text_document: TextDocumentIdentifier,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SemanticTokensRangeParams {
    text_document: TextDocumentIdentifier,
    range: Range,
}

fn invalid_params(id: Option<Value>) -> Dispatch {
    match id {
        Some(id) => Dispatch::messages(vec![rpc_error(id, -32602, "invalid params")]),
        None => Dispatch::none(),
    }
}

fn rpc_result(id: Value, result: Value) -> Value {
    let mut response = json!({"jsonrpc": "2.0"});
    let object = response
        .as_object_mut()
        .expect("the static RPC response envelope is an object");
    object.insert("id".to_string(), id);
    object.insert("result".to_string(), result);
    response
}

fn rpc_notification(method: &str, params: Value) -> Value {
    let mut notification = json!({"jsonrpc": "2.0", "method": method});
    notification
        .as_object_mut()
        .expect("the static RPC notification envelope is an object")
        .insert("params".to_string(), params);
    notification
}

/// The workspace root from an `initialize` request: `rootUri`, else the first `workspaceFolders`
/// entry, else the deprecated `rootPath`.
fn workspace_root(params: &Value) -> Option<PathBuf> {
    params
        .get("rootUri")
        .and_then(Value::as_str)
        .and_then(file_uri_to_path)
        .or_else(|| {
            params
                .pointer("/workspaceFolders/0/uri")
                .and_then(Value::as_str)
                .and_then(file_uri_to_path)
        })
        .or_else(|| {
            params
                .get("rootPath")
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
}

fn client_supports_work_done_progress(params: &Value) -> bool {
    params
        .get("capabilities")
        .and_then(|c| c.get("window"))
        .and_then(|w| w.get("workDoneProgress"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// A `client/registerCapability` request for `workspace/didChangeWatchedFiles` over `globs`.
fn register_watched_files(globs: &[String]) -> Value {
    let watchers: Vec<Value> = globs
        .iter()
        .map(|glob| json!({ "globPattern": glob }))
        .collect();
    json!({
        "jsonrpc": "2.0",
        "id": "krusty/registerWatchers",
        "method": "client/registerCapability",
        "params": {
            "registrations": [{
                "id": "krusty/watchedFiles",
                "method": "workspace/didChangeWatchedFiles",
                "registerOptions": { "watchers": watchers },
            }],
        },
    })
}

fn show_message(kind: ProjectMessageKind, text: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "window/showMessage",
        "params": { "type": kind.message_type(), "message": text },
    })
}

fn log_message(text: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "window/logMessage",
        "params": { "type": 3, "message": text },
    })
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

fn diagnostic_server_cancelled(id: Value, message: &str, retrigger_request: bool) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32802,
            "message": message,
            "data": {"retriggerRequest": retrigger_request}
        }
    })
}

fn is_request_id(id: &Value) -> bool {
    match id {
        Value::Number(id) => id.is_i64() || id.is_u64(),
        Value::String(_) => true,
        _ => false,
    }
}

fn diagnostic_report(id: Value, diagnostics: Option<&DiagnosticIndex>) -> Value {
    let items = diagnostics.map(DiagnosticIndex::encode).unwrap_or_default();
    rpc_result(id, json!({"kind": "full", "items": items}))
}

fn publish_diagnostics(uri: &str, version: Option<i64>, diagnostics: &DiagnosticIndex) -> Value {
    let mut params = json!({"uri": uri});
    params["diagnostics"] = Value::Array(diagnostics.encode());
    if let Some(version) = version {
        params["version"] = json!(version);
    }
    rpc_notification("textDocument/publishDiagnostics", params)
}

/// IntelliJ's Kotlin LSP sentence-cases compiler diagnostics even though kotlinc's CLI renderer keeps
/// the same message lowercase. Do this only at the protocol boundary so compiler diagnostics remain
/// byte-for-byte compatible with kotlinc. Current Kotlin diagnostic prefixes are ASCII; mutating that
/// byte in place avoids another allocation in the analysis-to-wire path.
fn lsp_diagnostic_message(mut message: String) -> String {
    if let Some(first_byte) = message.get_mut(..1) {
        first_byte.make_ascii_uppercase();
    }
    message
}

fn analysis_limit_diagnostics() -> DiagnosticIndex {
    DiagnosticIndex::from_diagnostics(
        vec![Diagnostic {
            span: krusty::diag::Span::new(0, 0),
            editor_span: None,
            severity: Severity::Error,
            kind: DiagnosticKind::Compiler,
            msg: format!(
                "workspace analysis limit exceeded (maximum {} MiB of open source, {} MiB per analysis group, and {} open documents)",
                MAX_OPEN_SOURCE_BYTES / (1024 * 1024),
                MAX_SOURCE_SET_BYTES / (1024 * 1024),
                MAX_OPEN_DOCUMENTS
            ),
            file: 0,
        }],
        "",
        &mut DiagnosticBudget::default(),
    )
}

/// Read one LSP `Content-Length` framed message while bounding the input allocation.
pub fn read_framed<R: BufRead>(reader: &mut R, max_bytes: usize) -> io::Result<Option<Vec<u8>>> {
    let mut content_length = None;
    let mut header_bytes = 0usize;
    loop {
        let remaining = MAX_HEADER_BYTES.saturating_sub(header_bytes);
        let mut line = Vec::new();
        let read = reader
            .take((remaining + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return if header_bytes == 0 {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated LSP header",
                ))
            };
        }
        header_bytes = header_bytes.saturating_add(read);
        if header_bytes > MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP header too large",
            ));
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        let line = std::str::from_utf8(&line)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 LSP header"))?;
        let Some((name, value)) = line.split_once(':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed LSP header",
            ));
        };
        if name.eq_ignore_ascii_case("Content-Length") {
            content_length = Some(value.trim().parse::<usize>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length")
            })?);
        }
    }

    let length = content_length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    if length > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LSP message too large",
        ));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

pub fn write_framed<W: Write>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

/// Serve one LSP connection until `exit` or input EOF.
pub fn run_connection<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<i32> {
    run_connection_with(reader, writer, DocumentAnalyzer)
}

/// Serve one LSP connection with a caller-provided semantic analysis platform.
pub fn run_connection_with<R, W, A>(reader: &mut R, writer: &mut W, analyze: A) -> io::Result<i32>
where
    R: BufRead,
    W: Write,
    A: Analysis,
{
    let mut service = LspService::new(analyze);
    loop {
        let Some(body) = read_framed(reader, MAX_MESSAGE_BYTES)? else {
            return Ok(0);
        };
        let message = match serde_json::from_slice::<Value>(&body) {
            Ok(message) => message,
            Err(_) => {
                let response = rpc_error(Value::Null, -32700, "parse error");
                let encoded = serde_json::to_vec(&response).map_err(json_io)?;
                write_framed(writer, &encoded)?;
                continue;
            }
        };
        // The parsed value owns all strings needed by dispatch. Release the raw frame before
        // compiler analysis constructs its AST and type tables.
        drop(body);

        let dispatch = service.handle(message);
        for response in dispatch.messages {
            let encoded = serde_json::to_vec(&response).map_err(json_io)?;
            write_framed(writer, &encoded)?;
        }
        if dispatch.exit {
            return Ok(dispatch.exit_code);
        }
    }
}

pub(crate) enum Incoming {
    Message(Value),
    ParseError,
    Error(io::Error),
    Eof,
    Engine(EngineEvent),
}

fn change_identity(message: &Value) -> Option<(&str, i64)> {
    if message.get("method")?.as_str()? != "textDocument/didChange" {
        return None;
    }
    Some((
        message
            .pointer("/params/textDocument/uri")
            .and_then(Value::as_str)?,
        message
            .pointer("/params/textDocument/version")
            .and_then(Value::as_i64)?,
    ))
}

fn is_single_full_document_change(message: &Value) -> bool {
    let Some(changes) = message
        .pointer("/params/contentChanges")
        .and_then(Value::as_array)
    else {
        return false;
    };
    changes.len() == 1
        && changes[0].get("text").and_then(Value::as_str).is_some()
        && changes[0].get("range").is_none_or(Value::is_null)
}

fn document_notification_identity(message: &Value) -> Option<(&str, &str)> {
    let method = message.get("method")?.as_str()?;
    let uri = match method {
        "textDocument/didOpen" => message
            .pointer("/params/textDocument/uri")
            .and_then(Value::as_str)?,
        "textDocument/didChange" => change_identity(message)?.0,
        "textDocument/didClose" => message
            .pointer("/params/textDocument/uri")
            .and_then(Value::as_str)?,
        _ => return None,
    };
    Some((method, uri))
}

fn retained_value_bytes(value: &Value) -> usize {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => 16,
        Value::String(text) => 24usize.saturating_add(text.capacity()),
        Value::Array(values) => values.iter().fold(24usize, |total, value| {
            total.saturating_add(retained_value_bytes(value))
        }),
        Value::Object(values) => values.iter().fold(48usize, |total, (key, value)| {
            total
                .saturating_add(24)
                .saturating_add(key.capacity())
                .saturating_add(retained_value_bytes(value))
        }),
    }
}

pub(crate) fn coalesce_document_notifications(
    message: Value,
    incoming: &Receiver<Incoming>,
    pending: &mut VecDeque<Incoming>,
) -> Vec<Value> {
    if document_notification_identity(&message).is_none() {
        return vec![message];
    }
    let deadline = Instant::now() + MAX_BATCH_DURATION;
    let mut retained_bytes = retained_value_bytes(&message);
    let mut changes = vec![message];
    while changes.len() < MAX_BATCH_MESSAGES {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match incoming.recv_timeout(CHANGE_DEBOUNCE.min(remaining)) {
            Ok(Incoming::Message(next)) if document_notification_identity(&next).is_some() => {
                let next_bytes = retained_value_bytes(&next);
                if next_bytes > MAX_BATCH_VALUE_BYTES.saturating_sub(retained_bytes) {
                    pending.push_back(Incoming::Message(next));
                    break;
                }
                let next_change =
                    change_identity(&next).map(|(uri, version)| (uri.to_owned(), version));
                let replace = is_single_full_document_change(&next)
                    .then_some(next_change.as_ref())
                    .flatten()
                    .and_then(|(next_uri, next_version)| {
                        changes
                            .last()
                            .filter(|change| {
                                document_notification_identity(change)
                                    .is_some_and(|(_, uri)| uri == next_uri)
                            })
                            .filter(|change| change_identity(change).is_some())
                            .map(|_| (changes.len() - 1, *next_version))
                    });
                match replace {
                    Some((index, next_version)) => {
                        let (_, current_version) = change_identity(&changes[index]).unwrap();
                        if next_version > current_version {
                            retained_bytes = retained_bytes
                                .saturating_sub(retained_value_bytes(&changes[index]))
                                .saturating_add(next_bytes);
                            changes[index] = next;
                        }
                    }
                    None => {
                        retained_bytes = retained_bytes.saturating_add(next_bytes);
                        changes.push(next);
                    }
                }
            }
            Ok(other) => {
                pending.push_back(other);
                break;
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        }
    }
    changes
}

fn dispatch_messages<W: Write>(writer: &mut W, dispatch: Dispatch) -> io::Result<Option<i32>> {
    for response in dispatch.messages {
        let encoded = serde_json::to_vec(&response).map_err(json_io)?;
        write_framed(writer, &encoded)?;
    }
    if dispatch.exit {
        Ok(Some(dispatch.exit_code))
    } else {
        Ok(None)
    }
}

pub(super) fn dispatch_document_batch<W, B>(
    writer: &mut W,
    service: &mut LspService<B>,
    changes: Vec<Value>,
) -> io::Result<Option<i32>>
where
    W: Write,
    B: AnalysisBackend,
{
    for change in changes {
        if let Some(code) = dispatch_messages(writer, service.handle_deferred(change))? {
            return Ok(Some(code));
        }
    }
    dispatch_messages(writer, Dispatch::messages(service.flush_analysis()))
}

/// Production stdio loop. Input framing/parsing runs on a bounded reader queue so document-state
/// bursts can be applied together before invoking the compiler worker.
pub fn run_stdio_connection_with<A>(analyze: A) -> io::Result<i32>
where
    A: Analysis,
{
    let (sender, incoming) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        loop {
            let event = match read_framed(&mut reader, MAX_MESSAGE_BYTES) {
                Ok(Some(body)) => match serde_json::from_slice::<Value>(&body) {
                    Ok(message) => Incoming::Message(message),
                    Err(_) => Incoming::ParseError,
                },
                Ok(None) => Incoming::Eof,
                Err(error) => Incoming::Error(error),
            };
            let terminal = matches!(event, Incoming::Eof | Incoming::Error(_));
            if sender.send(event).is_err() || terminal {
                break;
            }
        }
    });

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let mut service = LspService::new(analyze);
    let mut pending = VecDeque::new();
    let mut input_dispatches_since_maintenance = 0usize;
    loop {
        if maintenance_preempts_input(
            input_dispatches_since_maintenance,
            service.project_refresh_due_in(),
        ) {
            for message in service.run_due_project_refresh() {
                let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                write_framed(&mut writer, &encoded)?;
            }
            input_dispatches_since_maintenance = 0;
            continue;
        }
        let event = match pending.pop_front() {
            Some(event) => event,
            None => match service.project_refresh_due_in() {
                Some(due) => match incoming.recv_timeout(due) {
                    Ok(event) => event,
                    Err(RecvTimeoutError::Timeout) => {
                        for message in service.run_due_project_refresh() {
                            let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                            write_framed(&mut writer, &encoded)?;
                        }
                        input_dispatches_since_maintenance = 0;
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => Incoming::Eof,
                },
                None => incoming.recv().unwrap_or(Incoming::Eof),
            },
        };
        let messages = match event {
            Incoming::Message(message) => {
                coalesce_document_notifications(message, &incoming, &mut pending)
            }
            Incoming::ParseError => {
                let response = rpc_error(Value::Null, -32700, "parse error");
                let encoded = serde_json::to_vec(&response).map_err(json_io)?;
                write_framed(&mut writer, &encoded)?;
                input_dispatches_since_maintenance =
                    input_dispatches_since_maintenance.saturating_add(1);
                continue;
            }
            Incoming::Error(error) => return Err(error),
            Incoming::Eof => return Ok(0),
            Incoming::Engine(_) => Vec::new(),
        };
        let result = if messages.len() > 1
            || messages
                .first()
                .is_some_and(|m| document_notification_identity(m).is_some())
        {
            dispatch_document_batch(&mut writer, &mut service, messages)?
        } else {
            dispatch_messages(
                &mut writer,
                service.handle(messages.into_iter().next().unwrap()),
            )?
        };
        if let Some(code) = result {
            return Ok(code);
        }
        input_dispatches_since_maintenance = input_dispatches_since_maintenance.saturating_add(1);
    }
}

fn handle_engine_event<W, B>(
    service: &mut LspService<B>,
    writer: &mut W,
    event: EngineEvent,
) -> io::Result<()>
where
    W: Write,
    B: AnalysisBackend,
{
    if service.shutdown_requested {
        return Ok(());
    }
    match event {
        EngineEvent::ReadyState(ready) => {
            service.set_backend_ready(ready);
            if ready {
                service.mark_analysis_dirty();
            }
        }
        EngineEvent::WatchedGlobs(globs) => {
            if globs.is_empty() {
                return Ok(());
            }
            if service.client_initialized() {
                let message = register_watched_files(&globs);
                let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                write_framed(writer, &encoded)?;
            } else {
                service.defer_watched_globs(globs);
            }
        }
        EngineEvent::Project(feedback) => {
            let reanalyze = feedback.reanalyze;
            for message in service.project_feedback_messages(feedback) {
                let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                write_framed(writer, &encoded)?;
            }
            if reanalyze {
                service.mark_analysis_dirty();
            }
        }
        EngineEvent::ReanalyzeRequested => service.mark_analysis_dirty(),
        EngineEvent::AnalysisComplete(batch) => {
            for message in service.apply_analysis_batch(batch) {
                let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                write_framed(writer, &encoded)?;
            }
        }
        EngineEvent::Status(status) => {
            for message in service.status.report(status) {
                let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                write_framed(writer, &encoded)?;
            }
        }
        EngineEvent::Materialized(result) => {
            if let Some(message) = service.complete_materialization(result) {
                let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                write_framed(writer, &encoded)?;
            }
        }
    }
    Ok(())
}

fn step_async<W, B>(
    service: &mut LspService<B>,
    writer: &mut W,
    incoming: &Receiver<Incoming>,
    pending: &mut VecDeque<Incoming>,
    event: Incoming,
) -> io::Result<Option<i32>>
where
    W: Write,
    B: AnalysisBackend,
{
    match event {
        Incoming::Message(message) => {
            for change in coalesce_document_notifications(message, incoming, pending) {
                if let Some(code) = dispatch_messages(writer, service.handle_deferred(change))? {
                    return Ok(Some(code));
                }
            }
        }
        Incoming::ParseError => {
            let response = rpc_error(Value::Null, -32700, "parse error");
            let encoded = serde_json::to_vec(&response).map_err(json_io)?;
            write_framed(writer, &encoded)?;
        }
        Incoming::Error(error) => return Err(error),
        Incoming::Eof => return Ok(Some(0)),
        Incoming::Engine(engine_event) => handle_engine_event(service, writer, engine_event)?,
    }
    for message in service.submit_pending_analysis() {
        let encoded = serde_json::to_vec(&message).map_err(json_io)?;
        write_framed(writer, &encoded)?;
    }
    Ok(None)
}

pub fn run_stdio_connection_async<A>(analyze: A) -> io::Result<i32>
where
    A: Analysis + Send + 'static,
{
    let (sender, incoming) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
    let engine_events = sender.clone();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        loop {
            let event = match read_framed(&mut reader, MAX_MESSAGE_BYTES) {
                Ok(Some(body)) => match serde_json::from_slice::<Value>(&body) {
                    Ok(message) => Incoming::Message(message),
                    Err(_) => Incoming::ParseError,
                },
                Ok(None) => Incoming::Eof,
                Err(error) => Incoming::Error(error),
            };
            let terminal = matches!(event, Incoming::Eof | Incoming::Error(_));
            if sender.send(event).is_err() || terminal {
                break;
            }
        }
    });

    let engine = AnalysisEngine::spawn(analyze, engine_events);
    let backend = EngineBackend::new(engine, false);
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let service = LspService::with_backend(backend);
    run_async_loop(service, &mut writer, incoming)
}

fn run_async_loop<W>(
    mut service: LspService<EngineBackend>,
    writer: &mut W,
    incoming: Receiver<Incoming>,
) -> io::Result<i32>
where
    W: Write,
{
    let mut pending = VecDeque::new();
    let mut input_dispatches_since_maintenance = 0usize;
    let outcome = loop {
        if maintenance_preempts_input(
            input_dispatches_since_maintenance,
            service.project_refresh_due_in(),
        ) {
            for message in service.run_due_project_refresh_deferred() {
                let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                write_framed(writer, &encoded)?;
            }
            for message in service.submit_pending_analysis() {
                let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                write_framed(writer, &encoded)?;
            }
            input_dispatches_since_maintenance = 0;
            continue;
        }
        let event = match pending.pop_front() {
            Some(event) => event,
            None => match service.project_refresh_due_in() {
                Some(due) => match incoming.recv_timeout(due) {
                    Ok(event) => event,
                    Err(RecvTimeoutError::Timeout) => {
                        for message in service.run_due_project_refresh_deferred() {
                            let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                            write_framed(writer, &encoded)?;
                        }
                        for message in service.submit_pending_analysis() {
                            let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                            write_framed(writer, &encoded)?;
                        }
                        input_dispatches_since_maintenance = 0;
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => Incoming::Eof,
                },
                None => incoming.recv().unwrap_or(Incoming::Eof),
            },
        };
        match step_async(&mut service, writer, &incoming, &mut pending, event) {
            Ok(Some(code)) => break Ok(code),
            Ok(None) => {
                input_dispatches_since_maintenance =
                    input_dispatches_since_maintenance.saturating_add(1);
                continue;
            }
            Err(error) => break Err(error),
        }
    };
    for message in service.status.finish() {
        if let Ok(encoded) = serde_json::to_vec(&message) {
            let _ = write_framed(writer, &encoded);
        }
    }
    let mut engine = service.backend.into_engine();
    engine.disconnect();
    while !engine.is_finished() {
        let _ = incoming.recv_timeout(Duration::from_millis(50));
    }
    engine.join();
    outcome
}

fn maintenance_preempts_input(input_dispatches: usize, due: Option<Duration>) -> bool {
    input_dispatches >= MAX_INPUT_DISPATCHES_BEFORE_MAINTENANCE
        && due.is_some_and(|due| due.is_zero())
}

fn json_io(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::engine::{EngineCommand, EngineEvent, ServerStatus};

    fn decode_messages(bytes: &[u8]) -> Vec<Value> {
        let mut reader = io::Cursor::new(bytes);
        let mut messages = Vec::new();
        while let Some(body) = read_framed(&mut reader, MAX_MESSAGE_BYTES).unwrap() {
            messages.push(serde_json::from_slice(&body).unwrap());
        }
        messages
    }

    struct RecordingBackend {
        ready: bool,
        submitted: std::rc::Rc<std::cell::RefCell<Vec<AnalysisJob>>>,
    }

    impl AnalysisBackend for RecordingBackend {
        fn analysis_ready(&self) -> bool {
            self.ready
        }
        fn submit(&mut self, job: AnalysisJob) -> Option<AnalysisBatch> {
            self.submitted.borrow_mut().push(job);
            None
        }
        fn set_workspace_root(&mut self, _root: Option<PathBuf>) -> Option<ProjectFeedback> {
            None
        }
        fn watched_globs(&mut self) -> Vec<String> {
            Vec::new()
        }
        fn note_project_change(&mut self) {}
        fn note_watched_file_change(&mut self, _uri: &str) -> bool {
            false
        }
        fn project_refresh_due_in(&self) -> Option<Duration> {
            None
        }
        fn refresh_project(&mut self) -> Option<ProjectFeedback> {
            None
        }
        fn set_ready(&mut self, ready: bool) {
            self.ready = ready;
        }
    }

    struct CountingBackend {
        batch_calls: std::rc::Rc<std::cell::Cell<usize>>,
        single_calls: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl AnalysisBackend for CountingBackend {
        fn analysis_ready(&self) -> bool {
            true
        }
        fn submit(&mut self, _job: AnalysisJob) -> Option<AnalysisBatch> {
            None
        }
        fn set_workspace_root(&mut self, _root: Option<PathBuf>) -> Option<ProjectFeedback> {
            None
        }
        fn watched_globs(&mut self) -> Vec<String> {
            Vec::new()
        }
        fn note_project_change(&mut self) {}
        fn note_watched_file_change(&mut self, _uri: &str) -> bool {
            self.single_calls.set(self.single_calls.get() + 1);
            true
        }
        fn note_watched_file_changes(&mut self, _uris: &[String]) -> bool {
            self.batch_calls.set(self.batch_calls.get() + 1);
            false
        }
        fn project_refresh_due_in(&self) -> Option<Duration> {
            None
        }
        fn refresh_project(&mut self) -> Option<ProjectFeedback> {
            None
        }
    }

    #[test]
    fn watched_file_changes_are_coalesced_into_one_backend_submit() {
        let batch_calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let single_calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut service = LspService::with_backend(CountingBackend {
            batch_calls: batch_calls.clone(),
            single_calls: single_calls.clone(),
        });
        service.force_initialized_for_test();

        let changes: Vec<Value> = (0..20)
            .map(|i| json!({ "uri": format!("file:///p/File{i}.kt") }))
            .collect();
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "workspace/didChangeWatchedFiles",
            "params": { "changes": changes }
        });
        let _ = service.handle_deferred(notification);

        assert_eq!(
            batch_calls.get(),
            1,
            "20 changes coalesce to one backend submit per notification"
        );
        assert_eq!(
            single_calls.get(),
            0,
            "the per-uri single-submit path must not be used on this route"
        );
    }

    #[test]
    fn async_loop_applies_completion_and_dispatches_next() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let backend = RecordingBackend {
            ready: false,
            submitted: submitted.clone(),
        };
        let mut service = LspService::with_backend(backend);
        service.force_initialized_for_test();

        let (tx, incoming) = mpsc::sync_channel::<Incoming>(1);
        drop(tx);
        let mut pending = VecDeque::new();
        let mut out: Vec<u8> = Vec::new();

        let did_open = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": "file:///a.kt", "languageId": "kotlin", "version": 1, "text": "fun a(){}"
            }}
        });
        step_async(
            &mut service,
            &mut out,
            &incoming,
            &mut pending,
            Incoming::Message(did_open),
        )
        .unwrap();
        assert!(submitted.borrow().is_empty());

        step_async(
            &mut service,
            &mut out,
            &incoming,
            &mut pending,
            Incoming::Engine(EngineEvent::ReadyState(true)),
        )
        .unwrap();
        assert_eq!(
            submitted.borrow().len(),
            1,
            "opening a document dispatches one analysis job"
        );
        let version = {
            let jobs = submitted.borrow();
            jobs[0].documents[0].2
        };

        let batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), version)],
            analyses: vec![DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        };
        step_async(
            &mut service,
            &mut out,
            &incoming,
            &mut pending,
            Incoming::Engine(EngineEvent::AnalysisComplete(batch)),
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("textDocument/publishDiagnostics"));
        assert!(text.contains("file:///a.kt"));
        assert_eq!(submitted.borrow().len(), 1);
    }

    #[test]
    fn watcher_registration_waits_for_initialized_notification() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let backend = RecordingBackend {
            ready: false,
            submitted,
        };
        let mut service = LspService::with_backend(backend);
        let mut out = Vec::new();

        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        dispatch_messages(&mut out, service.handle(initialize)).unwrap();
        out.clear();

        handle_engine_event(
            &mut service,
            &mut out,
            EngineEvent::WatchedGlobs(vec!["**/build.gradle.kts".into()]),
        )
        .unwrap();
        handle_engine_event(
            &mut service,
            &mut out,
            EngineEvent::Project(ProjectFeedback {
                logs: vec!["project loaded".into()],
                ..ProjectFeedback::default()
            }),
        )
        .unwrap();
        assert!(out.is_empty());

        let initialized = json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });
        dispatch_messages(&mut out, service.handle(initialized)).unwrap();
        let output = String::from_utf8(out).unwrap();
        assert!(output.contains("client/registerCapability"));
        assert!(output.contains("project loaded"));
    }

    #[test]
    fn pending_async_batch_retries_after_backoff() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "fun a() {}", 1);
        service.mark_analysis_dirty_for_test();
        service
            .dispatch_pending_analysis()
            .expect("initial analysis job");

        let messages = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: Vec::new(),
            support_documents: Vec::new(),
            pending: true,
        });
        assert!(messages.is_empty());
        assert!(service.analysis_retry_at.is_some());
        assert!(!service.analysis_dirty_for_test());
        assert!(!service.analysis_in_flight_for_test());

        service.make_analysis_retry_due();
        service.run_due_project_refresh_deferred();
        assert!(service.dispatch_pending_analysis().is_some());
    }

    #[test]
    fn async_loop_emits_status_notifications() {
        let backend = RecordingBackend {
            ready: false,
            submitted: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
        };
        let mut service = LspService::with_backend(backend);
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {
                    "window": { "workDoneProgress": true }
                }
            }
        });
        let mut out = Vec::new();
        dispatch_messages(&mut out, service.handle(initialize)).unwrap();
        out.clear();

        let (tx, incoming) = mpsc::sync_channel::<Incoming>(1);
        drop(tx);
        let mut pending = VecDeque::new();

        for status in [
            ServerStatus::Working("Loading project".into()),
            ServerStatus::Working("Analyzing 3 files".into()),
            ServerStatus::Ready,
        ] {
            step_async(
                &mut service,
                &mut out,
                &incoming,
                &mut pending,
                Incoming::Engine(EngineEvent::Status(status)),
            )
            .unwrap();
        }

        let messages = decode_messages(&out);
        let shapes: Vec<(String, Option<String>)> = messages
            .iter()
            .map(|m| {
                let method = m["method"].as_str().unwrap_or_default().to_string();
                let kind = m["params"]["value"]["kind"]
                    .as_str()
                    .map(ToString::to_string);
                (method, kind)
            })
            .collect();
        assert_eq!(
            shapes,
            vec![
                ("window/workDoneProgress/create".to_string(), None),
                ("$/progress".to_string(), Some("begin".to_string())),
                ("$/progress".to_string(), Some("report".to_string())),
                ("$/progress".to_string(), Some("end".to_string())),
            ],
            "create, then begin/report/end in order: {messages:?}"
        );
    }

    #[test]
    fn engine_thread_is_joined_on_eof() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        struct Mock {
            _flag: DropFlag,
        }
        impl Analysis for Mock {
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let mock = Mock {
            _flag: DropFlag(dropped.clone()),
        };

        let (sender, incoming) = mpsc::sync_channel(8);
        let engine = AnalysisEngine::spawn(mock, sender.clone());
        let service = LspService::with_backend(EngineBackend::new(engine, false));

        sender.send(Incoming::Eof).unwrap();

        let mut out: Vec<u8> = Vec::new();
        let code = run_async_loop(service, &mut out, incoming).unwrap();

        assert_eq!(code, 0);
        assert!(
            dropped.load(Ordering::SeqCst),
            "the engine thread must be joined on EOF, dropping the owned Analysis"
        );
    }

    #[test]
    fn shutdown_ends_open_status() {
        struct Mock;
        impl Analysis for Mock {
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
        }

        let (sender, incoming) = mpsc::sync_channel(8);
        let engine = AnalysisEngine::spawn(Mock, sender.clone());
        let mut service = LspService::with_backend(EngineBackend::new(engine, false));
        service.force_initialized_for_test();
        service.status.set_supported(true);

        sender
            .send(Incoming::Engine(EngineEvent::Status(
                ServerStatus::Working("Loading project".into()),
            )))
            .unwrap();
        sender.send(Incoming::Eof).unwrap();

        let mut out: Vec<u8> = Vec::new();
        let code = run_async_loop(service, &mut out, incoming).unwrap();
        assert_eq!(code, 0);

        let messages = decode_messages(&out);
        assert!(
            messages
                .iter()
                .any(|m| m["method"] == "$/progress" && m["params"]["value"]["kind"] == "end"),
            "shutdown must end the still-open transient token: {messages:?}"
        );
    }

    #[test]
    fn shutdown_drains_a_backlog_of_events_without_deadlocking() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        struct Mock {
            _flag: DropFlag,
        }
        impl Analysis for Mock {
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let mock = Mock {
            _flag: DropFlag(dropped.clone()),
        };

        let (sender, incoming) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
        let engine = AnalysisEngine::spawn(mock, sender.clone());

        sender.send(Incoming::Eof).unwrap();

        engine.submit(EngineCommand::SetWorkspaceRoot(None));
        engine.submit(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///a.kt".into(), "fun a(){}".into(), 1)],
            open_uris: vec!["file:///a.kt".into()],
        }));
        engine.submit(EngineCommand::ProjectChange {
            refresh: false,
            reanalyze: true,
            uris: Vec::new(),
        });

        let service = LspService::with_backend(EngineBackend::new(engine, false));

        let mut out: Vec<u8> = Vec::new();
        let code = run_async_loop(service, &mut out, incoming).unwrap();

        assert_eq!(code, 0);
        assert!(
            dropped.load(Ordering::SeqCst),
            "shutdown must drain the backlog and join the engine thread instead of deadlocking"
        );
    }

    #[test]
    fn request_is_answered_while_analysis_is_in_flight() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc::{channel, sync_channel};
        use std::sync::Arc;

        struct BlockingAnalysis {
            entered: std::sync::mpsc::Sender<()>,
            release: std::sync::mpsc::Receiver<()>,
            completed: Arc<AtomicBool>,
        }
        impl Analysis for BlockingAnalysis {
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                let _ = self.entered.send(());
                let _ = self.release.recv_timeout(Duration::from_secs(5));
                self.completed.store(true, Ordering::SeqCst);
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
        }

        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        let completed = Arc::new(AtomicBool::new(false));
        let mock = BlockingAnalysis {
            entered: entered_tx,
            release: release_rx,
            completed: completed.clone(),
        };

        let (sender, incoming) = sync_channel::<Incoming>(INPUT_QUEUE_CAPACITY);
        let engine = AnalysisEngine::spawn(mock, sender.clone());
        let backend = EngineBackend::new(engine, true);
        let mut service = LspService::with_backend(backend);
        service.force_initialized_for_test();

        let mut pending = VecDeque::new();
        let mut out: Vec<u8> = Vec::new();

        let ready = incoming.recv().expect("engine startup ready-state");
        step_async(&mut service, &mut out, &incoming, &mut pending, ready).unwrap();

        let did_open = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": "file:///a.kt", "languageId": "kotlin", "version": 1, "text": "fun a() {}"
            }}
        });
        step_async(
            &mut service,
            &mut out,
            &incoming,
            &mut pending,
            Incoming::Message(did_open),
        )
        .unwrap();

        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("analysis started on the engine thread");
        assert!(
            !completed.load(Ordering::SeqCst),
            "analysis must still be in flight"
        );

        let before = out.len();
        let hover = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/hover",
            "params": {
                "textDocument": {"uri": "file:///a.kt"},
                "position": {"line": 0, "character": 1}
            }
        });
        step_async(
            &mut service,
            &mut out,
            &incoming,
            &mut pending,
            Incoming::Message(hover),
        )
        .unwrap();

        let answered = String::from_utf8(out[before..].to_vec()).unwrap();
        assert!(
            answered.contains("\"id\":2"),
            "hover response must be written while analysis is in flight: {answered:?}"
        );
        assert!(
            !completed.load(Ordering::SeqCst),
            "the hover was answered before analysis completed"
        );

        release_tx.send(()).unwrap();
        let mut published = false;
        for _ in 0..INPUT_QUEUE_CAPACITY + 2 {
            let event = incoming
                .recv_timeout(Duration::from_secs(5))
                .expect("analysis completion event");
            step_async(&mut service, &mut out, &incoming, &mut pending, event).unwrap();
            if String::from_utf8_lossy(&out).contains("textDocument/publishDiagnostics") {
                published = true;
                break;
            }
        }
        assert!(published, "analysis result is published after release");
        assert!(completed.load(Ordering::SeqCst));
    }

    #[test]
    fn repeated_pull_diagnostics_keep_one_bounded_full_report() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "bad", 1);
        service.mark_analysis_dirty_for_test();
        let _job = service
            .dispatch_pending_analysis()
            .expect("analysis starts for the open document");

        for id in 0..4 {
            let dispatch = service.pull_diagnostics(
                Some(json!(id)),
                json!({"textDocument": {"uri": "file:///a.kt"}}),
            );
            if id == 0 {
                assert!(
                    dispatch.messages.is_empty(),
                    "the first current-version pull waits for analysis"
                );
            } else {
                assert_eq!(dispatch.messages.len(), 1);
                assert_eq!(dispatch.messages[0]["id"], id - 1);
                assert_eq!(dispatch.messages[0]["error"]["code"], -32802);
                assert_eq!(
                    dispatch.messages[0]["error"]["data"]["retriggerRequest"], false,
                    "a newer queued pull supersedes the older request"
                );
            }
            assert_eq!(
                service.pending_diagnostic_requests.len(),
                1,
                "duplicate pulls must never retain duplicate full reports"
            );
        }
        assert!(service.pending_diagnostic_request_bytes <= MAX_PENDING_DIAGNOSTIC_REQUEST_BYTES);

        let mut spare_capacity_id = String::with_capacity(MAX_PENDING_DIAGNOSTIC_REQUEST_BYTES + 1);
        spare_capacity_id.push('x');
        let overflow = service.pull_diagnostics(
            Some(Value::String(spare_capacity_id)),
            json!({"textDocument": {"uri": "file:///a.kt"}}),
        );
        assert_eq!(overflow.messages.len(), 1);
        assert_eq!(overflow.messages[0]["id"], "x");
        assert_eq!(overflow.messages[0]["error"]["code"], -32802);
        assert_eq!(
            overflow.messages[0]["error"]["data"]["retriggerRequest"],
            true
        );
        assert_eq!(service.pending_diagnostic_requests.len(), 1);
        assert!(service.pending_diagnostic_request_bytes > 0);

        let queued = service.pull_diagnostics(
            Some(json!(999)),
            json!({"textDocument": {"uri": "file:///a.kt"}}),
        );
        assert_eq!(queued.messages.len(), 1);
        assert_eq!(queued.messages[0]["id"], 3);
        let invalid_id = service.handle(json!({
            "jsonrpc": "2.0",
            "id": {"not": "a JSON-RPC request id"},
            "method": "textDocument/diagnostic",
            "params": {"textDocument": {"uri": "file:///a.kt"}}
        }));
        assert_eq!(invalid_id.messages[0]["id"], Value::Null);
        assert_eq!(invalid_id.messages[0]["error"]["code"], -32600);
        assert_eq!(
            service.pending_diagnostic_requests.len(),
            1,
            "an invalid request must not displace the valid queued pull"
        );
        let null_id = service.handle(json!({
            "jsonrpc": "2.0",
            "id": null,
            "method": "textDocument/diagnostic",
            "params": {"textDocument": {"uri": "file:///a.kt"}}
        }));
        assert_eq!(null_id.messages[0]["id"], Value::Null);
        assert_eq!(null_id.messages[0]["error"]["code"], -32600);
        assert_eq!(service.pending_diagnostic_requests.len(), 1);

        let maximum_message = "x".repeat(MAX_SOURCE_SET_DIAGNOSTIC_TEXT_BYTES);
        let batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis::with_diagnostics(vec![Diagnostic {
                span: krusty::diag::Span::new(0, 3),
                editor_span: None,
                severity: Severity::Error,
                kind: DiagnosticKind::Compiler,
                msg: maximum_message,
                file: 0,
            }])],
            support_documents: Vec::new(),
            pending: false,
        };
        let messages = service.apply_analysis_batch(batch);
        assert_eq!(
            messages.len(),
            2,
            "one publish plus one full response, independent of repeated pulls"
        );
        assert_eq!(messages[1]["id"], 999);
        assert_eq!(
            messages[1]["result"]["items"][0]["message"]
                .as_str()
                .unwrap()
                .len(),
            MAX_SOURCE_SET_DIAGNOSTIC_TEXT_BYTES
        );
        assert!(service.pending_diagnostic_requests.is_empty());
        assert_eq!(service.pending_diagnostic_request_bytes, 0);
    }

    #[test]
    fn client_cancellation_completes_a_pending_diagnostic_request() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "bad", 1);
        service.mark_analysis_dirty_for_test();
        let _job = service
            .dispatch_pending_analysis()
            .expect("analysis starts for the open document");
        assert!(service
            .pull_diagnostics(
                Some(json!("pull-1")),
                json!({"textDocument": {"uri": "file:///a.kt"}}),
            )
            .messages
            .is_empty());

        let cancelled = service.handle(json!({
            "jsonrpc": "2.0",
            "method": "$/cancelRequest",
            "params": {"id": "pull-1"}
        }));

        assert_eq!(cancelled.messages.len(), 1);
        assert_eq!(cancelled.messages[0]["id"], "pull-1");
        assert_eq!(cancelled.messages[0]["error"]["code"], -32800);
        assert!(service.pending_diagnostic_requests.is_empty());
        assert_eq!(service.pending_diagnostic_request_bytes, 0);
    }

    #[test]
    fn document_change_cancels_a_pending_diagnostic_request() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "bad", 1);
        service.mark_analysis_dirty_for_test();
        let _job = service
            .dispatch_pending_analysis()
            .expect("analysis starts for the open document");
        assert!(service
            .pull_diagnostics(
                Some(json!(7)),
                json!({"textDocument": {"uri": "file:///a.kt"}}),
            )
            .messages
            .is_empty());

        let changed = service.did_change(
            None,
            json!({
                "textDocument": {"uri": "file:///a.kt", "version": 2},
                "contentChanges": [{"text": "good"}]
            }),
            true,
        );

        assert_eq!(changed.messages.len(), 1);
        assert_eq!(changed.messages[0]["id"], 7);
        assert_eq!(changed.messages[0]["error"]["code"], -32802);
        assert_eq!(
            changed.messages[0]["error"]["data"]["retriggerRequest"],
            true
        );
        assert!(service.pending_diagnostic_requests.is_empty());
        assert_eq!(service.pending_diagnostic_request_bytes, 0);
    }

    #[test]
    fn pending_diagnostic_requests_share_the_byte_budget() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        for uri in ["file:///a.kt", "file:///b.kt", "file:///c.kt"] {
            service.open_document_for_test(uri, "bad", 1);
        }
        service.mark_analysis_dirty_for_test();
        let _job = service
            .dispatch_pending_analysis()
            .expect("analysis starts for the open documents");

        let id_bytes = MAX_PENDING_DIAGNOSTIC_REQUEST_BYTES / 3;
        for (uri, id) in [
            ("file:///a.kt", "a".repeat(id_bytes)),
            ("file:///b.kt", "b".repeat(id_bytes)),
        ] {
            let queued = service.pull_diagnostics(
                Some(Value::String(id)),
                json!({"textDocument": {"uri": uri}}),
            );
            assert!(queued.messages.is_empty());
        }

        let rejected = service.pull_diagnostics(
            Some(Value::String("c".repeat(id_bytes))),
            json!({"textDocument": {"uri": "file:///c.kt"}}),
        );
        assert_eq!(rejected.messages.len(), 1);
        assert_eq!(rejected.messages[0]["error"]["code"], -32802);
        assert_eq!(service.pending_diagnostic_requests.len(), 2);
        assert!(service.pending_diagnostic_request_bytes <= MAX_PENDING_DIAGNOSTIC_REQUEST_BYTES);
    }

    #[test]
    fn shutdown_cancels_pending_diagnostic_pulls_before_responding() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "bad", 1);
        service.mark_analysis_dirty_for_test();
        let _job = service
            .dispatch_pending_analysis()
            .expect("analysis starts for the open document");
        assert!(service
            .pull_diagnostics(
                Some(json!(7)),
                json!({"textDocument": {"uri": "file:///a.kt"}}),
            )
            .messages
            .is_empty());

        let shutdown = service.handle(json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "shutdown",
            "params": null
        }));
        assert_eq!(shutdown.messages.len(), 2);
        assert_eq!(shutdown.messages[0]["id"], 7);
        assert_eq!(shutdown.messages[0]["error"]["code"], -32802);
        assert_eq!(
            shutdown.messages[0]["error"]["data"]["retriggerRequest"],
            false
        );
        assert_eq!(shutdown.messages[1], rpc_result(json!(8), Value::Null));
        assert!(service.pending_diagnostic_requests.is_empty());
        assert_eq!(service.pending_diagnostic_request_bytes, 0);

        let exited = service.handle(json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }));
        assert!(exited.exit);
        assert_eq!(exited.exit_code, 0);
    }

    #[test]
    fn mixed_stale_batch_discards_entire_batch() {
        let mut service = LspService::new(|s: &[&str]| {
            s.iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "v1", 1);
        service.open_document_for_test("file:///b.kt", "v1", 1);
        service.mark_analysis_dirty_for_test();
        let _job = service
            .dispatch_pending_analysis()
            .expect("job for the open documents");

        service.open_document_for_test("file:///b.kt", "v2", 2);

        let batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1), ("file:///b.kt".into(), 1)],
            analyses: vec![DocumentAnalysis::empty(), DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        };
        let messages = service.apply_analysis_batch(batch);
        assert!(messages.is_empty());
        assert!(service.analysis_dirty_for_test());
        assert!(!service.analysis_in_flight_for_test());
    }

    #[test]
    fn stale_analysis_does_not_complete_a_current_diagnostic_pull() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "old", 1);
        service.mark_analysis_dirty_for_test();
        let _old_job = service
            .dispatch_pending_analysis()
            .expect("old-version analysis");
        assert!(service
            .pull_diagnostics(
                Some(json!(7)),
                json!({"textDocument": {"uri": "file:///a.kt"}}),
            )
            .messages
            .is_empty());

        service.open_document_for_test("file:///a.kt", "new", 2);
        let stale = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        });
        assert!(stale.is_empty());
        assert_eq!(service.pending_diagnostic_requests.len(), 1);

        let _current_job = service
            .dispatch_pending_analysis()
            .expect("current-version analysis");
        let current = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 2)],
            analyses: vec![DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        });
        assert_eq!(current.len(), 2, "publish plus the queued pull response");
        assert_eq!(current[1]["id"], 7);
        assert!(service.pending_diagnostic_requests.is_empty());
    }

    #[test]
    fn overdue_maintenance_gets_a_bounded_turn_amid_input() {
        assert!(!maintenance_preempts_input(
            MAX_INPUT_DISPATCHES_BEFORE_MAINTENANCE - 1,
            Some(Duration::ZERO)
        ));
        assert!(!maintenance_preempts_input(
            MAX_INPUT_DISPATCHES_BEFORE_MAINTENANCE,
            Some(Duration::from_millis(1))
        ));
        assert!(maintenance_preempts_input(
            MAX_INPUT_DISPATCHES_BEFORE_MAINTENANCE,
            Some(Duration::ZERO)
        ));
    }

    #[test]
    fn stale_batch_document_is_discarded() {
        let mut service = LspService::new(|s: &[&str]| {
            s.iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "v1", 1);
        let job = service.take_analysis_job();
        assert_eq!(job.documents[0].2, 1);

        service.open_document_for_test("file:///a.kt", "v2", 2);

        let batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        };
        let messages = service.apply_analysis_batch(batch);
        assert!(messages.is_empty());
        assert!(service.analysis_dirty_for_test());
    }

    #[test]
    fn fresh_batch_document_is_applied() {
        let mut service = LspService::new(|s: &[&str]| {
            s.iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "v1", 1);
        service.take_analysis_job();
        let batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        };
        let messages = service.apply_analysis_batch(batch);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["method"], "textDocument/publishDiagnostics");
    }

    #[test]
    fn close_reopen_with_reused_version_discards_in_flight_batch() {
        let mut service = LspService::new(|s: &[&str]| {
            s.iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();

        let with_diagnostic = || DocumentAnalysis {
            diagnostics: vec![Diagnostic {
                span: krusty::diag::Span::new(0, 0),
                editor_span: None,
                severity: Severity::Error,
                kind: DiagnosticKind::Compiler,
                msg: "from stale batch".to_string(),
                file: 0,
            }],
            ..DocumentAnalysis::empty()
        };
        let did_open = |uri: &str, text: &str, version: i64| {
            serde_json::json!({
                "textDocument": { "uri": uri, "languageId": "kotlin", "version": version, "text": text }
            })
        };

        service.did_open(None, did_open("file:///a.kt", "old", 1), true);
        let in_flight_job = service
            .dispatch_pending_analysis()
            .expect("job for the freshly opened document");
        assert_eq!(in_flight_job.documents[0].2, 1);
        assert!(service.analysis_in_flight_for_test());

        service.did_close(
            None,
            serde_json::json!({ "textDocument": { "uri": "file:///a.kt" } }),
            true,
        );
        service.did_open(None, did_open("file:///a.kt", "new", 1), true);

        let stale_batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![with_diagnostic()],
            support_documents: Vec::new(),
            pending: false,
        };
        let messages = service.apply_analysis_batch(stale_batch);
        assert!(
            messages.is_empty(),
            "stale close+reopen batch must not publish diagnostics"
        );
        assert_eq!(
            service.document_diagnostic_count_for_test("file:///a.kt"),
            0,
            "stale analysis must not populate the reopened document's indices"
        );
        assert!(!service.analysis_in_flight_for_test());
        assert!(service.analysis_dirty_for_test());
        assert!(
            !service.resubmit_pending_for_test(),
            "discard clears the resubmit slot; a fresh job re-dispatches via analysis_dirty"
        );

        let fresh_job = service
            .dispatch_pending_analysis()
            .expect("fresh job re-dispatches after discard");
        assert_eq!(fresh_job.documents[0].1, "new");
        let fresh_batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![with_diagnostic()],
            support_documents: Vec::new(),
            pending: false,
        };
        let messages = service.apply_analysis_batch(fresh_batch);
        assert_eq!(messages.len(), 1, "fresh batch is applied");
        assert_eq!(messages[0]["method"], "textDocument/publishDiagnostics");
        assert_eq!(
            service.document_diagnostic_count_for_test("file:///a.kt"),
            1,
            "fresh analysis populates the reopened document"
        );
    }

    #[test]
    fn analysis_coalesces_to_one_in_flight() {
        let mut service = LspService::new(|s: &[&str]| {
            s.iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "v1", 1);
        service.mark_analysis_dirty_for_test();

        let first = service.dispatch_pending_analysis();
        assert!(first.is_some(), "first dispatch runs");

        service.mark_analysis_dirty_for_test();
        let second = service.dispatch_pending_analysis();
        assert!(second.is_none(), "in-flight → coalesced");
        assert!(service.resubmit_pending_for_test());

        let batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        };
        let _ = service.apply_analysis_batch(batch);
        assert!(!service.analysis_in_flight_for_test());
        let third = service.dispatch_pending_analysis();
        assert!(third.is_some(), "resubmit dispatches after completion");
    }

    #[test]
    fn stale_batch_clears_resubmit_pending_and_stays_dirty() {
        let mut service = LspService::new(|s: &[&str]| {
            s.iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "v1", 1);
        service.mark_analysis_dirty_for_test();

        let first = service.dispatch_pending_analysis();
        assert!(first.is_some(), "first dispatch runs");

        service.mark_analysis_dirty_for_test();
        let second = service.dispatch_pending_analysis();
        assert!(second.is_none(), "in-flight → coalesced");
        assert!(service.resubmit_pending_for_test());

        service.open_document_for_test("file:///a.kt", "v2", 2);

        let batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        };
        let _ = service.apply_analysis_batch(batch);

        assert!(!service.resubmit_pending_for_test());
        assert!(!service.analysis_in_flight_for_test());
        assert!(service.analysis_dirty_for_test());
    }

    #[test]
    fn rename_diff_matches_the_official_delete_on_tie_edits_and_bounds_work() {
        let changes = |old, new| {
            rename_text_changes(old, new)
                .unwrap()
                .into_iter()
                .map(|change| (change.old_lo, change.old_hi, change.new_text))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            changes("answer", "renamedAnswer"),
            [(0, 0, "ren".to_string()), (1, 1, "medA".to_string())]
        );
        assert_eq!(
            changes("answer", "renamedLocal"),
            [
                (0, 1, "re".to_string()),
                (2, 4, "am".to_string()),
                (5, 6, "dLocal".to_string()),
            ]
        );
        assert_eq!(
            changes("`odd name`", "plainName"),
            [
                (0, 5, "plai".to_string()),
                (6, 6, "N".to_string()),
                (9, 10, String::new()),
            ]
        );
        assert_eq!(
            changes("😀target", "renamedTarget"),
            [(0, 5, "ren".to_string()), (6, 6, "medTa".to_string()),]
        );
        assert!(rename_text_changes("x", &"n".repeat(MAX_RENAME_IDENTIFIER_BYTES + 1)).is_none());
    }

    #[test]
    fn owned_rpc_envelopes_move_array_storage_without_cloning() {
        let result_items = vec![json!({"message": "result diagnostic"})];
        let result_storage = result_items.as_ptr();
        let response = rpc_result(json!(7), Value::Array(result_items));
        assert_eq!(
            response["result"].as_array().unwrap().as_ptr(),
            result_storage
        );

        let params_items = vec![json!({"message": "published diagnostic"})];
        let params_storage = params_items.as_ptr();
        let mut params = json!({"uri": "file:///main.kt"});
        params["diagnostics"] = Value::Array(params_items);
        let notification = rpc_notification("textDocument/publishDiagnostics", params);
        assert_eq!(
            notification["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .as_ptr(),
            params_storage
        );
    }

    #[test]
    fn diagnostic_index_is_compact_interned_and_source_set_bounded() {
        assert_eq!(std::mem::size_of::<DiagnosticEntry>(), 20);
        let diagnostics = vec![
            Diagnostic {
                span: krusty::diag::Span::new(0, 1),
                editor_span: None,
                severity: Severity::Error,
                kind: DiagnosticKind::Compiler,
                msg: "same message".to_string(),
                file: 0,
            },
            Diagnostic {
                span: krusty::diag::Span::new(2, 3),
                editor_span: None,
                severity: Severity::Warning,
                kind: DiagnosticKind::Compiler,
                msg: "same message".to_string(),
                file: 0,
            },
        ];
        let mut budget = DiagnosticBudget::default();
        let index = DiagnosticIndex::from_diagnostics(diagnostics, "a\nb", &mut budget);
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.messages, ["Same message"]);
        assert_eq!(budget.entries, 2);
        assert_eq!(budget.text_bytes, "Same message".len());
        assert_eq!(
            budget.wire_bytes,
            2 * (DIAGNOSTIC_WIRE_FIXED_BYTES + json_string_wire_bytes("Same message"))
        );
        assert_eq!(
            Value::Array(index.encode()),
            json!([
                {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 1}
                    },
                    "severity": 1,
                    "source": "Kotlin",
                    "message": "Same message"
                },
                {
                    "range": {
                        "start": {"line": 1, "character": 0},
                        "end": {"line": 1, "character": 1}
                    },
                    "severity": 2,
                    "source": "Kotlin",
                    "message": "Same message"
                }
            ])
        );

        let diagnostic = || Diagnostic {
            span: krusty::diag::Span::new(0, 0),
            editor_span: None,
            severity: Severity::Error,
            kind: DiagnosticKind::Compiler,
            msg: "bounded".to_string(),
            file: 0,
        };
        let mut entry_limited = DiagnosticBudget {
            entries: MAX_SOURCE_SET_DIAGNOSTIC_ENTRIES,
            text_bytes: 0,
            wire_bytes: 0,
        };
        assert!(
            DiagnosticIndex::from_diagnostics(vec![diagnostic()], "", &mut entry_limited)
                .entries
                .is_empty()
        );
        let mut text_limited = DiagnosticBudget {
            entries: 0,
            text_bytes: MAX_SOURCE_SET_DIAGNOSTIC_TEXT_BYTES,
            wire_bytes: 0,
        };
        assert!(
            DiagnosticIndex::from_diagnostics(vec![diagnostic()], "", &mut text_limited)
                .entries
                .is_empty()
        );
        let mut wire_limited = DiagnosticBudget {
            entries: 0,
            text_bytes: 0,
            wire_bytes: MAX_SOURCE_SET_DIAGNOSTIC_WIRE_BYTES,
        };
        assert!(
            DiagnosticIndex::from_diagnostics(vec![diagnostic()], "", &mut wire_limited)
                .entries
                .is_empty()
        );
    }

    #[test]
    fn diagnostic_positions_are_resolved_once_and_expanded_output_is_bounded() {
        let prefix = "x".repeat(256 * 1024);
        let source = format!("{prefix}\r\n😀tail");
        let emoji = u32::try_from(prefix.len() + 2).unwrap();
        let diagnostics = (0..4096)
            .map(|_| Diagnostic {
                span: krusty::diag::Span::new(emoji, emoji + 4),
                editor_span: None,
                severity: Severity::Error,
                kind: DiagnosticKind::Compiler,
                msg: "late source diagnostic".to_string(),
                file: 0,
            })
            .collect();
        let mut position_budget = DiagnosticBudget::default();
        let index = DiagnosticIndex::from_diagnostics(diagnostics, &source, &mut position_budget);
        assert_eq!(index.entries.len(), 4096);
        assert!(index.entries.iter().all(|entry| entry[..4] == [1, 0, 1, 2]));

        let large_message = "m".repeat(128 * 1024);
        let repeated = (0..100)
            .map(|_| Diagnostic {
                span: krusty::diag::Span::new(0, 0),
                editor_span: None,
                severity: Severity::Warning,
                kind: DiagnosticKind::Compiler,
                msg: large_message.clone(),
                file: 0,
            })
            .collect();
        let mut output_budget = DiagnosticBudget::default();
        let bounded = DiagnosticIndex::from_diagnostics(repeated, "", &mut output_budget);
        assert_eq!(bounded.messages.len(), 1);
        assert!(!bounded.entries.is_empty());
        assert!(bounded.entries.len() < 100);
        assert!(output_budget.wire_bytes <= MAX_SOURCE_SET_DIAGNOSTIC_WIRE_BYTES);
        let encoded = serde_json::to_vec(&Value::Array(bounded.encode())).unwrap();
        assert!(encoded.len() <= MAX_SOURCE_SET_DIAGNOSTIC_WIRE_BYTES);
    }

    #[test]
    fn a_file_uri_decodes_percent_escapes_and_drops_the_scheme() {
        assert_eq!(
            file_uri_to_path("file:///home/qnox/pro%20ject"),
            Some(PathBuf::from("/home/qnox/pro ject"))
        );
    }

    #[test]
    fn a_non_local_file_authority_is_not_treated_as_a_local_path() {
        assert_eq!(file_uri_to_path("file://host/srv/code"), None);
        assert_eq!(file_uri_to_path("untitled:Untitled-1"), None);
        assert_eq!(file_uri_to_path("file://"), None);
    }

    #[test]
    fn the_workspace_root_prefers_root_uri_then_folders_then_root_path() {
        assert_eq!(
            workspace_root(&json!({ "rootUri": "file:///a" })),
            Some(PathBuf::from("/a"))
        );
        assert_eq!(
            workspace_root(&json!({
                "rootUri": "untitled:workspace",
                "workspaceFolders": [{ "uri": "file:///b" }]
            })),
            Some(PathBuf::from("/b"))
        );
        assert_eq!(
            workspace_root(&json!({ "rootPath": "/c" })),
            Some(PathBuf::from("/c"))
        );
        assert_eq!(workspace_root(&json!({})), None);
    }

    #[test]
    fn incremental_scan_budget_rolls_back_prior_edits() {
        let changes = vec![
            ContentChange {
                text: "x".to_string(),
                range: Some(Range {
                    start: Position::new(0, 0),
                    end: Position::new(0, 1),
                }),
                range_length: None,
            },
            ContentChange {
                text: "y".to_string(),
                range: Some(Range {
                    start: Position::new(0, 4),
                    end: Position::new(0, 4),
                }),
                range_length: None,
            },
        ];
        let mut budget = ContentChangeBudget::new();
        budget.scan_bytes = 3;

        assert_eq!(
            apply_content_changes_with_budget("abcd".to_string(), changes, budget),
            Err("abcd".to_string())
        );
    }

    #[test]
    fn incremental_edit_budget_rolls_back_prior_edits() {
        let changes = vec![
            ContentChange {
                text: "x".to_string(),
                range: Some(Range {
                    start: Position::new(0, 0),
                    end: Position::new(0, 0),
                }),
                range_length: None,
            },
            ContentChange {
                text: "y".to_string(),
                range: Some(Range {
                    start: Position::new(0, 0),
                    end: Position::new(0, 0),
                }),
                range_length: None,
            },
        ];
        let mut budget = ContentChangeBudget::new();
        budget.edit_bytes = 8;

        assert_eq!(
            apply_content_changes_with_budget("abcd".to_string(), changes, budget),
            Err("abcd".to_string())
        );
    }

    #[test]
    fn full_change_collapses_prior_undo_fragments() {
        let changes = vec![
            ContentChange {
                text: String::new(),
                range: Some(Range {
                    start: Position::new(0, 0),
                    end: Position::new(0, 4),
                }),
                range_length: None,
            },
            ContentChange {
                text: "replacement".to_string(),
                range: None,
                range_length: None,
            },
        ];
        let mut budget = ContentChangeBudget::new();
        budget.undo_bytes = 4;

        assert_eq!(
            apply_content_changes_with_budget("abcd".to_string(), changes, budget),
            Ok("replacement".to_string())
        );
    }

    #[test]
    fn inline_backend_submit_returns_batch_immediately() {
        let mut backend = InlineBackend::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        let batch = backend.submit(crate::server::engine::AnalysisJob {
            documents: vec![("file:///a.kt".into(), "fun a(){}".into(), 1)],
            open_uris: vec!["file:///a.kt".into()],
        });
        let batch = batch.expect("inline backend is synchronous");
        assert_eq!(batch.analyzed, vec![("file:///a.kt".to_string(), 1)]);
    }

    #[test]
    fn parses_work_done_progress_capability() {
        let params = json!({
            "capabilities": {
                "window": { "workDoneProgress": true }
            }
        });
        assert!(client_supports_work_done_progress(&params));
        assert!(!client_supports_work_done_progress(&json!({})));
    }
}
