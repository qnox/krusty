//! JSON-RPC/LSP session state and bounded stdio dispatch.
//!
//! This module lives in the separate `krusty-lsp` package, so the batch compiler neither links JSON
//! support nor retains server state. A session stores only the latest text and compact hover,
//! completion, navigation, and highlighting data for each open document; full compiler analysis is
//! dropped after every open/change notification.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::super::{
    CompletionIndex, DefinitionIndex, DependencyCandidate, DependencySymbolIndex, DocumentAnalysis,
    DocumentSymbolIndex, FoldingRangeIndex, HoverIndex, IndexOutcome, IndexedFile,
    LibraryDefinitionIndex, LocatedDependency, MaterializedDefinition, ProjectSymbolIndex,
    SemanticTokenIndex, SemanticTokenRange, SignatureHelpIndex, WorkspaceSymbolIndex,
    MAX_RETAINED_ANALYSIS_BYTES, MAX_WORKSPACE_SYMBOL_WIRE_BYTES, SEMANTIC_TOKEN_MODIFIERS,
    SEMANTIC_TOKEN_TYPES,
};
use super::workspace_index::{WorkspaceDiagnosticStore, WorkspaceDiagnostics};
use crate::analysis::serialized_json_wire_bytes;
use crate::compiler_analysis::LibraryRef;
use crate::server::engine::{
    AnalysisBatch, AnalysisEngine, AnalysisJob, DumpJob, DumpOutcome, DumpResult, EngineBackend,
    EngineEvent, IndexBatch, MaterializeJob, MaterializeResult, SymbolIndexBatch,
};
use crate::server::status::StatusReporter;
use crate::uri::{file_uri_to_path, path_to_file_uri};
use crate::worker::{source_set_fits, MAX_SOURCE_SET_BYTES};
use krusty::diag::{Diagnostic, DiagnosticKind, Severity};

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_HEADER_BYTES: usize = 8 * 1024;
const INPUT_QUEUE_CAPACITY: usize = 4;
const MAX_INPUT_DISPATCHES_BEFORE_MAINTENANCE: usize = 32;
/// How long shutdown waits for the analysis thread to notice the disconnect before
/// abandoning it. Without a bound, one wedged analysis keeps the process — and its
/// worker child — alive indefinitely after the client is gone.
const ENGINE_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
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
const MAX_PENDING_ANALYSIS_REQUEST_BYTES: usize = 256 * 1024;
/// Same bound as `MAX_PENDING_MATERIALIZATIONS`: a held-down keybinding must not grow the map of
/// dumps waiting on the analysis thread without limit.
const MAX_PENDING_DUMPS: usize = 128;
const BOUNDED_EXACT_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_WORKSPACE_DIAGNOSTIC_REPORTS: usize = 32 * 1024;
const MAX_RENAME_IDENTIFIER_BYTES: usize = 1024;
const MAX_RENAME_SPELLINGS: usize = 8;
pub(super) const MAX_RENAME_WIRE_BYTES: usize = 8 * 1024 * 1024;
const RENAME_DOCUMENT_WIRE_FIXED_BYTES: usize = 128;
const RENAME_EDIT_WIRE_FIXED_BYTES: usize = 192;
const MAX_FORMATTING_RESULT_BYTES: usize = BOUNDED_EXACT_RESPONSE_BYTES;
pub(super) const DIAGNOSTIC_WARNING_BIT: u32 = 1 << 31;
pub(super) const DIAGNOSTIC_INSPECTION_BIT: u32 = 1 << 30;
pub(super) const DIAGNOSTIC_MESSAGE_MASK: u32 =
    !(DIAGNOSTIC_WARNING_BIT | DIAGNOSTIC_INSPECTION_BIT);
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

    /// Index workspace files that are not open. Required rather than defaulted: a silently empty
    /// default let the whole background path look wired while producing nothing.
    fn index_workspace_files(&mut self, uris: &[&str]) -> IndexOutcome;

    /// Extract declarations from workspace files, whether or not anything has opened them.
    ///
    /// Defaulted, and the default reads and parses -- unlike [`Analysis::index_workspace_files`],
    /// there is nothing a host has to configure for this to be correct: symbol extraction needs no
    /// classpath, no module grouping, and no resolution, so every host gets real coverage.
    fn index_workspace_symbols(&mut self, uris: &[&str]) -> WorkspaceSymbolIndex {
        index_workspace_symbols_from_disk(uris)
    }

    /// Class names from the project's dependencies. Empty for a host with no classpath, which is
    /// every host that is not the real project one.
    fn dependency_index(&mut self) -> DependencySymbolIndex {
        DependencySymbolIndex::default()
    }

    /// Write out the source for `candidates` so a client can open them.
    fn locate_dependencies(
        &mut self,
        _candidates: Vec<DependencyCandidate>,
    ) -> Vec<LocatedDependency> {
        Vec::new()
    }

    /// Workspace sources sharing a module with one of the open documents. These are the files a
    /// change to the open set is most likely to affect, so they index ahead of the sweep.
    fn neighborhood_index_candidates(&mut self, _open_uris: &[&str]) -> Vec<String> {
        Vec::new()
    }

    /// Every workspace source that is a candidate for background indexing, as file URIs. Reuses
    /// the project model's own source inventory rather than walking the tree a second time.
    fn workspace_index_candidates(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// Whether the current model's inventory was truncated. Kept separate from the URI vector so
    /// queueing stays allocation-focused while the engine can report incomplete workspace coverage.
    fn workspace_index_incomplete(&self) -> bool {
        false
    }

    /// Install the reporter for workspace file-tree scan progress. The engine sets it once before
    /// its main loop; backends that never scan keep the default no-op.
    fn set_scan_reporter(&mut self, _reporter: crate::ScanReporter) {}

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

    /// Render the dev-mode AST/checker/IR dump for `uri` and report where it was written. `None`
    /// when dev mode is off, the document was not part of the retained analysis payload, or
    /// rendering failed.
    fn dump(&mut self, _uri: &str) -> Option<DumpResult> {
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

/// Read `uris` and extract their declarations, skipping anything unreadable.
///
/// The read is bounded before the allocation: a file whose size already exceeds the per-file cap is
/// never pulled into memory, so one generated multi-megabyte source costs a `stat` rather than a
/// parse. An unreadable or deleted file simply contributes nothing; the caller knows which URIs it
/// attempted and drops their stale entries.
pub fn index_workspace_symbols_from_disk(uris: &[&str]) -> WorkspaceSymbolIndex {
    // This check intentionally precedes the read: the generic builder never sees the source, so
    // an oversized file costs a `stat`. The skip is recorded with its URI after construction
    // rather than letting an empty-looking input incorrectly restore the chunk's completeness —
    // and so the client log can name the file instead of reporting an anonymous gap.
    let skipped = std::cell::RefCell::new(Vec::new());
    let readable = uris.iter().filter_map(|uri| {
        let path = crate::uri::file_uri_to_path(uri)?;
        let bytes = std::fs::metadata(&path).ok()?.len();
        let oversized = usize::try_from(bytes)
            .map_or(true, |size| size > crate::analysis::MAX_INDEXED_FILE_BYTES);
        if oversized {
            skipped.borrow_mut().push(((*uri).to_string(), bytes));
            return None;
        }
        Some((*uri, std::fs::read_to_string(path).ok()?))
    });
    let mut index = WorkspaceSymbolIndex::from_uri_sources(readable);
    for (uri, bytes) in skipped.into_inner() {
        index.note_oversized_file(&uri, bytes);
    }
    index
}

/// One log line saying what project-wide symbol search is missing and which limit took it.
///
/// Every clause carries the number a user can act on: the skipped file's URI and size against the
/// per-file cap, or the dropped-declaration count against the retention ceiling.
fn symbol_index_incomplete_message(project: &crate::analysis::ProjectSymbolIndex) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    let omissions = project.omissions();
    let mut clauses = Vec::new();
    if omissions.oversized_files > 0 {
        let mut examples = omissions
            .oversized_examples
            .iter()
            .map(|(uri, bytes)| format!("{uri} ({:.1} MiB)", *bytes as f64 / MIB))
            .collect::<Vec<_>>()
            .join(", ");
        if omissions.oversized_files > omissions.oversized_examples.len() {
            examples.push_str(", …");
        }
        clauses.push(format!(
            "skipped {} file(s) over the {} MiB per-file cap: {}",
            omissions.oversized_files,
            crate::analysis::MAX_INDEXED_FILE_BYTES / (1024 * 1024),
            examples,
        ));
    }
    if omissions.dropped_entries > 0 {
        clauses.push(format!(
            "dropped {} declaration(s) at the {} MiB retention ceiling ({} retained)",
            omissions.dropped_entries,
            crate::analysis::MAX_PROJECT_WORKSPACE_SYMBOL_INDEX_WIRE_BYTES / (1024 * 1024),
            project.entry_count(),
        ));
    }
    if omissions.truncated_chunks > 0 {
        clauses.push(format!(
            "{} index chunk(s) stopped early on a spent per-chunk budget",
            omissions.truncated_chunks,
        ));
    }
    if clauses.is_empty() {
        // Incomplete with no recorded provenance (e.g. a refused cross-phase merge).
        clauses.push("an index chunk could not be retained".to_string());
    }
    format!(
        "krusty: project-wide symbol search is incomplete: {}",
        clauses.join("; ")
    )
}

/// FNV-1a over the file text. Only ever compared against another hash this process produced, so a
/// non-cryptographic hash is the right trade.
pub fn workspace_text_hash(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

impl Analysis for DocumentAnalyzer {
    fn index_workspace_files(&mut self, uris: &[&str]) -> IndexOutcome {
        let readable: Vec<(String, String)> = uris
            .iter()
            .filter_map(|uri| {
                let path = url::Url::parse(uri).ok()?.to_file_path().ok()?;
                let text = std::fs::read_to_string(path).ok()?;
                Some(((*uri).to_string(), text))
            })
            .collect();
        if readable.is_empty() {
            // This standalone host has no infrastructure that can be pending: an empty read set is
            // a conclusive set of tombstones, not a failed analysis attempt.
            return IndexOutcome {
                files: Vec::new(),
                conclusive: true,
            };
        }
        let documents: Vec<(&str, &str)> = readable
            .iter()
            .map(|(uri, text)| (uri.as_str(), text.as_str()))
            .collect();
        let open: Vec<&str> = documents.iter().map(|(uri, _)| *uri).collect();
        let (analyses, _support) = self.analyze_open_documents(&documents, &open);
        let conclusive = analyses.len() == readable.len();
        let files = analyses
            .into_iter()
            .zip(readable)
            .map(|(analysis, (uri, text))| IndexedFile {
                uri,
                diagnostics: analysis.diagnostics,
                text_hash: workspace_text_hash(&text),
                text,
            })
            .collect();
        IndexOutcome { files, conclusive }
    }

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
    /// A bare closure analyses open documents only; workspace indexing needs the project model,
    /// which a closure does not carry.
    fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
        IndexOutcome::default()
    }

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

    /// Ask for dependency classes to be written out. Answers nothing: the query that wanted them
    /// has already been answered without them, and the next one reads what this produced.
    fn locate_dependencies(&mut self, _generation: u64, _candidates: Vec<DependencyCandidate>) {}
    /// Render the dev-mode dump for `job.uri`.
    ///
    /// Mirrors [`AnalysisBackend::materialize`]: `Some` answers the request now, `None` means the
    /// backend will publish an [`EngineEvent::Dumped`] carrying the same token later. The default
    /// answers immediately with nothing to open.
    fn dump(&mut self, job: DumpJob) -> Option<DumpOutcome> {
        Some(DumpOutcome {
            token: job.token,
            dump: None,
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

    fn dump(&mut self, job: DumpJob) -> Option<DumpOutcome> {
        let dump = self.0.dump(&job.uri);
        Some(DumpOutcome {
            token: job.token,
            dump,
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

    fn result_id(&self) -> String {
        let mut hasher = DefaultHasher::new();
        self.entries.hash(&mut hasher);
        self.messages.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    fn empty_result_id() -> String {
        DiagnosticIndex::default().result_id()
    }

    /// Entries are already resolved and messages already interned, so this is a copy rather than
    /// another position-resolution pass.
    fn from_workspace(found: &WorkspaceDiagnostics<'_>) -> DiagnosticIndex {
        let mut messages = Vec::new();
        let mut remapped = HashMap::<u32, u32>::new();
        let entries = found
            .entries
            .iter()
            .map(|entry| {
                let stored = entry[4] & DIAGNOSTIC_MESSAGE_MASK;
                let next = u32::try_from(messages.len()).unwrap_or(DIAGNOSTIC_MESSAGE_MASK);
                let local = *remapped.entry(stored).or_insert_with(|| {
                    messages.push(found.messages[stored as usize].clone());
                    next
                });
                let flags = entry[4] & !DIAGNOSTIC_MESSAGE_MASK;
                [entry[0], entry[1], entry[2], entry[3], flags | local]
            })
            .collect();
        DiagnosticIndex { entries, messages }
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

pub(super) fn resolve_diagnostic_positions(
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

enum PendingAnalysisRequestKind {
    Diagnostic {
        uri: String,
        previous_result_id: Option<String>,
    },
    Exact {
        method: String,
        params: Value,
    },
}

struct PendingAnalysisRequest {
    id: Value,
    kind: PendingAnalysisRequestKind,
    retained_bytes: usize,
}

impl PendingAnalysisRequest {
    fn uri(&self) -> Option<&str> {
        match &self.kind {
            PendingAnalysisRequestKind::Diagnostic { uri, .. } => Some(uri),
            PendingAnalysisRequestKind::Exact { params, .. } => {
                params.pointer("/textDocument/uri").and_then(Value::as_str)
            }
        }
    }

    fn exact_response_bytes(&self) -> usize {
        match &self.kind {
            PendingAnalysisRequestKind::Diagnostic { .. } => 0,
            PendingAnalysisRequestKind::Exact { method, .. } => {
                exact_analysis_response_bytes(method)
            }
        }
    }
}

#[derive(Clone, Copy)]
enum PendingAnalysisCancellation {
    Client,
    DocumentChanged,
    DocumentClosed,
    Shutdown,
}

const DIAGNOSTIC_REFRESH_REQUEST_ID: &str = "krusty/diagnosticRefresh";

/// Ceiling on dependency classes one response may carry.
///
/// Ranked below every project hit, so this is what is left over for names the workspace does not
/// declare. Small on purpose: a reader searching a project usually means the project, and each of
/// these costs a render the first time it is returned.
const MAX_DEPENDENCY_SYMBOLS_PER_RESPONSE: usize = 32;

/// Ceiling on rendered dependency locations held for reuse. Rendered text is reduced to UTF-16
/// endpoints as the engine event arrives and is not part of this long-lived count.
const MAX_LOCATED_DEPENDENCIES: usize = 512;

/// Session-owned form of a materialized dependency symbol.
///
/// The worker result carries source text because its byte span cannot be converted to protocol
/// coordinates without it. Conversion happens exactly once at the session boundary; retaining the
/// full text for hundreds of classes made an entry-count ceiling an ineffective memory bound.
struct RetainedDependencyLocation {
    path: PathBuf,
    start: Position,
    end: Position,
}

pub struct LspService<B> {
    documents: HashMap<String, OpenDocument>,
    source_set: Vec<(String, String)>,
    workspace_symbols: WorkspaceSymbolIndex,
    /// Declarations from every workspace file the background sweep has reached, opened or not.
    /// Unlike `workspace_symbols` this survives analysis batches: coverage is what it is for, and
    /// rebuilding it per batch would shrink it back to whatever happens to be open.
    project_symbols: ProjectSymbolIndex,
    /// Project-model generation `project_symbols` describes. A batch from an older generation is
    /// about a model that no longer exists.
    project_symbols_generation: u64,
    /// Which omission causes were already said out loud for this generation, as the
    /// `(oversized, dropped, truncated)` presence triple. A sweep grows the counts chunk by chunk,
    /// so re-reporting on every change would log per chunk; a report repeats only when a NEW kind
    /// of omission appears.
    reported_symbol_omissions: Option<(bool, bool, bool)>,
    /// Class names from the project's dependencies. Names only; a location costs a render.
    dependency_symbols: DependencySymbolIndex,
    /// Project-model generation shared by the dependency index and every asynchronous render.
    /// Without it, a queued result from the old classpath can repopulate the cache after reset.
    dependency_symbols_generation: u64,
    /// Dependency classes already written out, by internal name. A query answers from this and
    /// asks for what is missing, so it never waits on a render -- the picker re-queries on every
    /// keystroke, and the next one has them.
    located_dependencies: HashMap<String, RetainedDependencyLocation>,
    /// Candidates already asked for, so a keystroke does not queue the same render twice.
    requested_dependencies: HashSet<String>,
    workspace_diagnostics: WorkspaceDiagnosticStore,
    backend: B,
    analysis_dirty: bool,
    analysis_retry_at: Option<Instant>,
    analysis_retry_backoff: Duration,
    initialized: bool,
    client_initialized: bool,
    client_pulls_diagnostics: bool,
    client_refreshes_diagnostics: bool,
    diagnostic_refresh_pending: bool,
    diagnostic_refresh_queued: bool,
    shutdown_requested: bool,
    pending_init_feedback: Option<ProjectFeedback>,
    pending_watched_globs: Vec<String>,
    analysis_in_flight: bool,
    resubmit_pending: bool,
    changed_identities: HashSet<String>,
    pending_analysis_requests: VecDeque<PendingAnalysisRequest>,
    pending_analysis_request_bytes: usize,
    next_materialize_token: u64,
    pending_materializations: HashMap<u64, Value>,
    next_dump_token: u64,
    pending_dumps: HashMap<u64, PendingDump>,
    dev: bool,
    status: StatusReporter,
}

/// A `textDocument/codeAction` request waiting for the analysis thread to write its dump.
///
/// The originating URI and position are kept because the client-side navigation command needs them
/// in its argument list, and the request that carried them is long gone by the time the dump lands.
struct PendingDump {
    id: Value,
    uri: String,
    position: Position,
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
            workspace_symbols: WorkspaceSymbolIndex::default(),
            project_symbols: ProjectSymbolIndex::default(),
            project_symbols_generation: 0,
            reported_symbol_omissions: None,
            dependency_symbols: DependencySymbolIndex::default(),
            dependency_symbols_generation: 0,
            located_dependencies: HashMap::new(),
            requested_dependencies: HashSet::new(),
            workspace_diagnostics: WorkspaceDiagnosticStore::default(),
            backend,
            analysis_dirty: false,
            analysis_retry_at: None,
            analysis_retry_backoff: Duration::ZERO,
            initialized: false,
            client_initialized: false,
            client_pulls_diagnostics: false,
            client_refreshes_diagnostics: false,
            diagnostic_refresh_pending: false,
            diagnostic_refresh_queued: false,
            shutdown_requested: false,
            pending_init_feedback: None,
            pending_watched_globs: Vec::new(),
            analysis_in_flight: false,
            resubmit_pending: false,
            changed_identities: HashSet::new(),
            pending_analysis_requests: VecDeque::new(),
            pending_analysis_request_bytes: 0,
            next_materialize_token: 0,
            pending_materializations: HashMap::new(),
            next_dump_token: 0,
            pending_dumps: HashMap::new(),
            dev: false,
            status: StatusReporter::default(),
        }
    }

    /// Turn on the developer surfaces — currently the dump code action and the capability that
    /// advertises it. Off by default, so an ordinary session is unchanged.
    pub fn with_dev(mut self, dev: bool) -> Self {
        self.dev = dev;
        self
    }

    pub fn open_document_count(&self) -> usize {
        self.documents.len()
    }

    fn diagnostic_refresh(&mut self) -> Option<Value> {
        if !self.client_pulls_diagnostics || !self.client_refreshes_diagnostics {
            return None;
        }
        if self.diagnostic_refresh_pending {
            self.diagnostic_refresh_queued = true;
            return None;
        }
        self.diagnostic_refresh_pending = true;
        Some(json!({
            "jsonrpc": "2.0",
            "id": DIAGNOSTIC_REFRESH_REQUEST_ID,
            "method": "workspace/diagnostic/refresh",
        }))
    }

    fn pushes_diagnostics(&self) -> bool {
        // A client that supports pull diagnostics is expected to pull them. Sending the same
        // diagnostics through both push and pull channels makes editors such as Zed show them
        // twice, and the LSP model treats the two delivery modes as alternatives.
        !self.client_pulls_diagnostics
    }

    fn publish(
        &self,
        uri: &str,
        version: Option<i64>,
        diagnostics: &DiagnosticIndex,
    ) -> Option<Value> {
        self.pushes_diagnostics()
            .then(|| publish_diagnostics(uri, version, diagnostics))
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

    fn note_document_identity_change(&mut self, uri: &str) {
        if self.analysis_in_flight {
            self.changed_identities.insert(uri.to_owned());
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
        self.analysis_in_flight = false;
        let resubmit = std::mem::take(&mut self.resubmit_pending);
        let changed = std::mem::take(&mut self.changed_identities);
        let fresh = batch
            .analyzed
            .iter()
            .map(|(uri, analyzed_version)| {
                !changed.contains(uri)
                    && self
                        .documents
                        .get(uri)
                        .is_some_and(|open| open.version == *analyzed_version)
            })
            .collect::<Vec<_>>();
        if fresh.iter().any(|fresh| !fresh) {
            self.analysis_dirty = true;
        }
        if !fresh.is_empty() && !fresh.iter().any(|fresh| *fresh) {
            return Vec::new();
        }
        let batch_is_fresh = fresh.iter().all(|fresh| *fresh);
        let uris = batch
            .analyzed
            .iter()
            .zip(&fresh)
            .filter(|(_, fresh)| **fresh)
            .map(|((uri, _), _)| uri.clone())
            .collect::<Vec<_>>();
        if batch.pending {
            let current_uris = self
                .analyzed_uris()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            self.schedule_analysis_retry(&current_uris);
            return Vec::new();
        }
        let mut diagnostic_budget = DiagnosticBudget::default();
        if batch.analyses.len() != batch.analyzed.len() {
            if !batch_is_fresh {
                self.analysis_dirty = true;
                return Vec::new();
            }
            self.source_set.clear();
            self.workspace_symbols = WorkspaceSymbolIndex::default();
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
                        identity: None,
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
                .filter_map(|uri| {
                    let open = &self.documents[&uri];
                    self.publish(&uri, Some(open.version), &open.diagnostics)
                })
                .collect::<Vec<_>>();
            messages.extend(self.diagnostic_refresh());
            if !resubmit {
                messages.extend(self.complete_pending_analysis_requests());
            }
            return messages;
        }
        self.analysis_retry_at = None;
        self.analysis_retry_backoff = Duration::ZERO;
        let push = self.pushes_diagnostics();
        let mut messages = Vec::with_capacity(batch.analyses.len());
        let mut analyzed_documents = Vec::with_capacity(batch.analyzed.len());
        let mut workspace_symbols = WorkspaceSymbolIndex::default();
        for ((analysis, (uri, _analyzed_version)), fresh) in
            batch.analyses.into_iter().zip(batch.analyzed).zip(fresh)
        {
            if !fresh {
                continue;
            }
            let DocumentAnalysis {
                diagnostics,
                hover,
                completion,
                signature_help,
                semantic_tokens,
                definitions,
                type_definitions,
                implementations,
                library_definitions,
                document_symbols,
                workspace_symbols: document_workspace_symbols,
                folding_ranges,
                implementation_relations: _,
            } = analysis;
            if batch_is_fresh {
                workspace_symbols.merge_from(document_workspace_symbols);
            }
            let open = self
                .documents
                .get_mut(&uri)
                .expect("batch freshness checked before applying");
            open.hover = hover;
            open.completion = completion;
            open.signature_help = signature_help;
            open.semantic_tokens = semantic_tokens;
            open.library_definitions = library_definitions;
            open.document_symbols = document_symbols;
            open.folding_ranges = folding_ranges;
            if batch_is_fresh {
                open.definitions = definitions;
                open.type_definitions = type_definitions;
                open.implementations = implementations;
            }
            open.diagnostics =
                DiagnosticIndex::from_diagnostics(diagnostics, &open.text, &mut diagnostic_budget);
            if push {
                messages.push(publish_diagnostics(
                    &uri,
                    Some(open.version),
                    &open.diagnostics,
                ));
            }
            if batch_is_fresh {
                analyzed_documents.push((uri, open.text.clone()));
            }
        }
        if batch_is_fresh {
            self.source_set = analyzed_documents
                .into_iter()
                .chain(batch.support_documents)
                .collect();
            // The builder numbers entries by their position in the analyzed source set; this is the
            // first place that knows which document each position was. After binding the index
            // names its own files and no longer depends on the source set being retained.
            let uris = self
                .source_set
                .iter()
                .map(|(uri, _)| uri.as_str())
                .collect::<Vec<_>>();
            workspace_symbols.assign_uris(&uris);
            self.workspace_symbols = workspace_symbols;
        }
        messages.extend(self.diagnostic_refresh());
        if resubmit {
            self.analysis_dirty = true;
        }
        if !self.analysis_dirty {
            messages.extend(self.complete_pending_analysis_requests());
        }
        messages
    }

    pub(crate) fn apply_index_batch(&mut self, batch: IndexBatch) -> Vec<Value> {
        let IndexBatch {
            generation,
            attempted,
            files,
            conclusive,
        } = batch;
        let outcome = self.workspace_diagnostics.merge(
            generation,
            &attempted,
            conclusive,
            files,
            resolve_diagnostic_positions,
        );
        if !outcome.accepted {
            return Vec::new();
        }
        let mut messages = Vec::new();
        if outcome.newly_truncated {
            messages.push(log_message(
                "krusty: workspace diagnostic retention reached its memory limit; results are incomplete"
                    .to_string(),
            ));
        }
        if outcome.changed {
            // Publish each chunk as it lands when the client has no pull channel. A workspace
            // pull only arrives if the client asks, and on a large repository the sweep runs for
            // hours -- waiting until the end would show nothing for the whole of it. Open
            // documents are excluded: their buffer is newer than whatever the sweep read from disk.
            // Pull-capable clients are told to refresh instead; publishing the same diagnostics
            // through both channels makes editors such as Zed display them twice.
            if self.pushes_diagnostics() {
                for uri in attempted {
                    if self.documents.contains_key(&uri) {
                        continue;
                    }
                    let index = self
                        .workspace_diagnostics
                        .diagnostics(&uri)
                        .map(|found| DiagnosticIndex::from_workspace(&found))
                        .unwrap_or_default();
                    messages.push(publish_diagnostics(&uri, None, &index));
                }
            }
            messages.extend(self.diagnostic_refresh());
        }
        messages
    }

    /// Splice one chunk of the project-wide symbol index into what is already retained.
    ///
    /// Every attempted URI is re-indexed, so a file the chunk could not read loses its stale
    /// entries instead of keeping them forever.
    pub(crate) fn apply_symbol_index_batch(&mut self, batch: SymbolIndexBatch) -> Vec<Value> {
        if batch.generation < self.project_symbols_generation {
            return Vec::new();
        }
        self.project_symbols
            .replace_files(&batch.attempted, batch.symbols);
        // Said once per generation and cause. A file too large to parse or a layer at its
        // retention ceiling means the picker is answering over less than the workspace, and
        // nothing else would tell anyone: a missing symbol looks exactly like a symbol that does
        // not exist. The message names what was omitted and by which limit, because it is the
        // only place a user can learn which file to exclude or which ceiling is undersized.
        if self.project_symbols.is_complete() {
            return Vec::new();
        }
        let omissions = self.project_symbols.omissions();
        let causes = (
            omissions.oversized_files > 0,
            omissions.dropped_entries > 0,
            omissions.truncated_chunks > 0,
        );
        if self.reported_symbol_omissions.is_some_and(|reported| {
            (!causes.0 || reported.0) && (!causes.1 || reported.1) && (!causes.2 || reported.2)
        }) {
            return Vec::new();
        }
        self.reported_symbol_omissions = Some(causes);
        vec![log_message(symbol_index_incomplete_message(
            &self.project_symbols,
        ))]
    }

    pub(crate) fn set_dependency_index(&mut self, generation: u64, index: DependencySymbolIndex) {
        if generation != self.dependency_symbols_generation {
            return;
        }
        // A new index describes a new classpath. What was rendered from the old one may be a
        // different version of the same class, and serving it would open the wrong source; what
        // failed to render under the old one deserves another attempt.
        self.dependency_symbols = index;
        self.located_dependencies.clear();
        self.requested_dependencies.clear();
    }

    #[cfg(test)]
    fn set_dependency_index_for_test(&mut self, index: DependencySymbolIndex) {
        self.set_dependency_index(self.dependency_symbols_generation, index);
    }

    pub(crate) fn record_located_dependencies(
        &mut self,
        generation: u64,
        attempted: Vec<String>,
        located: Vec<LocatedDependency>,
    ) {
        if generation != self.dependency_symbols_generation {
            return;
        }
        // `requested_dependencies` means in flight, not permanently attempted. Release every
        // completed name, including failures, so a transient worker or cache error can be retried
        // by the next query. The previous success-only event never released an all-failed batch.
        for internal in attempted {
            self.requested_dependencies.remove(&internal);
        }
        for found in located {
            let start = byte_offset_to_position(&found.text, found.span.lo as usize);
            let end = byte_offset_to_position(&found.text, found.span.hi as usize);
            // Browsing widely still needs a count bound for paths and names. In-flight markers are
            // a separate correctness state and must survive: clearing them with the location cache
            // allowed duplicate engine work to be queued for requests already running.
            if self.located_dependencies.len() >= MAX_LOCATED_DEPENDENCIES
                && !self
                    .located_dependencies
                    .contains_key(&found.candidate.internal)
            {
                if let Some(evicted) = self.located_dependencies.keys().next().cloned() {
                    self.located_dependencies.remove(&evicted);
                }
            }
            let internal = found.candidate.internal.clone();
            self.located_dependencies.insert(
                internal,
                RetainedDependencyLocation {
                    path: found.path,
                    start,
                    end,
                },
            );
        }
    }

    #[cfg(test)]
    fn record_located_dependencies_for_test(&mut self, located: Vec<LocatedDependency>) {
        let attempted = located
            .iter()
            .map(|found| found.candidate.internal.clone())
            .collect();
        self.record_located_dependencies(self.dependency_symbols_generation, attempted, located);
    }

    /// Symbols for dependency classes matching `query`, and the candidates still to be written out.
    ///
    /// Answers from what has already been located and never blocks on the rest. A render is fast
    /// but it is still a render, and the picker asks again on the next keystroke -- by which time
    /// the missing ones have arrived. Ranked below every project hit, because a name the workspace
    /// declares is the one the reader meant.
    fn dependency_symbols(
        &self,
        query: &str,
        limit: usize,
        wire_bytes: &mut usize,
        remaining_glob_steps: &mut usize,
    ) -> (Vec<Value>, Vec<DependencyCandidate>) {
        let mut symbols = Vec::new();
        let mut missing = Vec::new();
        if query.is_empty() || limit == 0 {
            return (symbols, missing);
        }
        // The client re-filters results against the name it is given, so a qualified query has to
        // be answered with the qualified name -- exactly as the project layer does, or these hits
        // are computed and then discarded before anyone sees them.
        let parsed = crate::analysis::WorkspaceQuery::parse(query);
        for candidate in
            self.dependency_symbols
                .candidates_with_glob_steps(query, limit, remaining_glob_steps)
        {
            let Some(found) = self.located_dependencies.get(&candidate.internal) else {
                if !self.requested_dependencies.contains(&candidate.internal) {
                    missing.push(candidate);
                }
                continue;
            };
            let Some(uri) = path_to_file_uri(&found.path) else {
                continue;
            };
            let symbol = json!({
                "name": parsed.response_name(&candidate.package, &candidate.name),
                "kind": 5,
                "containerName": candidate.package,
                "location": {
                    "uri": uri,
                    "range": {"start": found.start, "end": found.end},
                },
            });
            let symbol_bytes = serialized_json_wire_bytes(&symbol).unwrap_or(usize::MAX);
            let next_bytes = wire_bytes.saturating_add(symbol_bytes).saturating_add(1);
            if next_bytes > MAX_WORKSPACE_SYMBOL_WIRE_BYTES {
                break;
            }
            *wire_bytes = next_bytes;
            symbols.push(symbol);
        }
        (symbols, missing)
    }

    pub(crate) fn reset_workspace_index(&mut self, generation: u64) -> Vec<Value> {
        self.project_symbols = ProjectSymbolIndex::default();
        self.project_symbols_generation = generation;
        self.reported_symbol_omissions = None;
        // Dependency names and materialized sources describe the same project model as the source
        // index. Clear all three pieces together; retaining names while clearing only locations
        // let stale candidates queue renders between reset and the replacement index arriving.
        self.dependency_symbols = DependencySymbolIndex::default();
        self.dependency_symbols_generation = generation;
        self.located_dependencies.clear();
        self.requested_dependencies.clear();
        self.workspace_diagnostics.reset_to(generation);
        // Clearing old-model results is itself a diagnostic change. Pull clients must be told even
        // when the replacement model produces no files or its first analysis is still pending.
        self.diagnostic_refresh().into_iter().collect()
    }

    fn schedule_analysis_retry(&mut self, uris: &[String]) {
        self.source_set.clear();
        self.workspace_symbols = WorkspaceSymbolIndex::default();
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
            if id.is_some() && (object.contains_key("result") || object.contains_key("error")) {
                if id
                    .as_ref()
                    .and_then(Value::as_str)
                    .is_some_and(|id| id == DIAGNOSTIC_REFRESH_REQUEST_ID)
                {
                    self.diagnostic_refresh_pending = false;
                    if std::mem::take(&mut self.diagnostic_refresh_queued) {
                        return Dispatch::messages(self.diagnostic_refresh().into_iter().collect());
                    }
                }
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
        let waits_for_analysis = if method == "workspace/symbol" {
            // Only an open document is worth waiting for. The retained snapshot no longer has to be
            // revalidated before it can be searched: the project-wide index covers files nothing
            // has opened, and the live layer shadows it for the ones that are open. Waiting on a
            // source set with no open documents left would park a query that the index can already
            // answer -- which is exactly what closing the last file used to do.
            !self.documents.is_empty()
                && (self.analysis_dirty
                    || self.analysis_in_flight
                    || self.resubmit_pending
                    || self.analysis_retry_at.is_some())
        } else if method == "textDocument/codeAction" && !self.dev {
            // The only action is the dev dump. An ordinary session must answer with its deliberate
            // empty list immediately rather than waiting for analysis that cannot change the result.
            false
        } else {
            exact_analysis_request_uri(&method, &params)
                .is_some_and(|uri| self.document_waits_for_analysis(uri))
        };
        if waits_for_analysis {
            let Some(id) = id else {
                return Dispatch::none();
            };
            return self.queue_exact_analysis_request(id, method, params);
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
                self.client_pulls_diagnostics = client_supports_pull_diagnostics(&params);
                self.client_refreshes_diagnostics = client_supports_diagnostic_refresh(&params);
                self.pending_init_feedback =
                    self.backend.set_workspace_root(workspace_root(&params));
                let mut capabilities = json!({
                    "hoverProvider": true,
                    "definitionProvider": true,
                    "typeDefinitionProvider": true,
                    "implementationProvider": true,
                    "referencesProvider": true,
                    "renameProvider": true,
                    "documentSymbolProvider": true,
                    "workspaceSymbolProvider": {
                        "resolveProvider": false,
                        "workDoneProgress": true,
                    },
                    "documentFormattingProvider": true,
                    "foldingRangeProvider": true,
                    "diagnosticProvider": {
                        "interFileDependencies": true,
                        "workspaceDiagnostics": true,
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
                });
                // Advertised only under `--dev`; an ordinary session must not offer the action.
                // Both entries are required: without `executeCommandProvider` the editor discards
                // the action instead of showing it, so advertising the provider alone is not enough.
                if self.dev {
                    capabilities["codeActionProvider"] = json!(true);
                    capabilities["executeCommandProvider"] =
                        json!({"commands": DUMP_ACTION_COMMANDS});
                }
                Dispatch::messages(vec![rpc_result(
                    id,
                    json!({
                        "capabilities": capabilities,
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
            "textDocument/codeAction" => self.code_action(id, params),
            "textDocument/typeDefinition" => self.type_definition(id, params),
            "textDocument/implementation" => self.implementation(id, params),
            "textDocument/references" => self.references(id, params),
            "textDocument/rename" => self.rename(id, params),
            "textDocument/documentSymbol" => self.document_symbols(id, params),
            "workspace/symbol" => self.workspace_symbols(id, params),
            "textDocument/formatting" => self.formatting(id, params),
            "textDocument/foldingRange" => self.folding_ranges(id, params),
            "textDocument/diagnostic" => self.pull_diagnostics(id, params),
            "workspace/diagnostic" => self.workspace_diagnostic(id, params),
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
                let mut messages = self.cancel_pending_analysis_requests(
                    |_| true,
                    PendingAnalysisCancellation::Shutdown,
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

    #[cfg(test)]
    pub(super) fn block_document_text_for_test(&mut self, uri: &str) {
        let open = self.documents.get_mut(uri).unwrap();
        open.text.clear();
        open.analysis_blocked = true;
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
            self.note_document_identity_change(&uri);
            self.cancel_pending_analysis_requests(
                |request| request.uri() == Some(uri.as_str()),
                PendingAnalysisCancellation::DocumentChanged,
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
            messages.extend(self.publish(&uri, Some(version), diagnostics));
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
        let push = self.pushes_diagnostics();
        let mut messages = self.cancel_pending_analysis_requests(
            |request| request.uri() == Some(uri.as_str()),
            PendingAnalysisCancellation::DocumentChanged,
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
            if push {
                messages.push(publish_diagnostics(
                    &uri,
                    Some(params.text_document.version),
                    &open.diagnostics,
                ));
            }
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
            self.note_document_identity_change(&uri);
        }
        // The live layer describes a buffer, and there is no buffer any more. Leaving it in place
        // would keep serving the abandoned text -- and keep shadowing the project layer's copy of
        // the file on disk -- until the replacement batch lands.
        self.workspace_symbols
            .remove_files(std::slice::from_ref(&uri));
        self.analysis_dirty = true;
        let mut messages = if defer_analysis {
            Vec::new()
        } else {
            self.flush_analysis()
        };
        messages.extend(self.publish(&uri, None, &DiagnosticIndex::default()));
        messages.extend(self.cancel_pending_analysis_requests(
            |request| request.uri() == Some(uri.as_str()),
            PendingAnalysisCancellation::DocumentClosed,
        ));
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

    fn workspace_symbols(&mut self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<WorkspaceSymbolParams>(params) else {
            return invalid_params(Some(id));
        };
        let mut remaining_glob_steps = crate::analysis::MAX_WORKSPACE_SYMBOL_GLOB_STEPS;
        let mut symbols = self.workspace_symbols.encode_over_with_glob_steps(
            &params.query,
            &self.project_symbols.layers(),
            &self.documents.keys().map(String::as_str).collect(),
            &mut remaining_glob_steps,
        );
        let mut wire_bytes = serialized_json_wire_bytes(&Value::Array(symbols.clone()))
            .unwrap_or(MAX_WORKSPACE_SYMBOL_WIRE_BYTES);
        // The response ceiling belongs to the composed response. Asking for 32 dependency renders
        // after the project layer already filled 512 slots did work whose results were immediately
        // truncated, contradicting the on-demand contract. Only the slots that can survive layer
        // composition are ranked and materialized.
        let dependency_limit = MAX_DEPENDENCY_SYMBOLS_PER_RESPONSE.min(
            crate::analysis::MAX_WORKSPACE_SYMBOL_RESPONSE_SYMBOLS.saturating_sub(symbols.len()),
        );
        let (dependencies, mut missing) = self.dependency_symbols(
            &params.query,
            dependency_limit,
            &mut wire_bytes,
            &mut remaining_glob_steps,
        );
        symbols.extend(dependencies);
        // The response ceiling covers the whole response, not each layer's share of it.
        symbols.truncate(crate::analysis::MAX_WORKSPACE_SYMBOL_RESPONSE_SYMBOLS);
        // In-flight candidates retain three owned names in both session state and the engine queue.
        // Bound them with the same ceiling as completed locations. Candidates not admitted here are
        // deliberately left unmarked, so a later query can try them after capacity is released.
        missing
            .truncate(MAX_LOCATED_DEPENDENCIES.saturating_sub(self.requested_dependencies.len()));
        for candidate in &missing {
            self.requested_dependencies
                .insert(candidate.internal.clone());
        }
        if !missing.is_empty() {
            self.backend
                .locate_dependencies(self.dependency_symbols_generation, missing);
        }
        Dispatch::messages(vec![rpc_result(id, Value::Array(symbols))])
    }

    fn formatting(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<DocumentFormattingParams>(params) else {
            return invalid_params(Some(id));
        };
        let Some(open) = self
            .documents
            .get(&params.text_document.uri)
            .filter(|open| !open.analysis_blocked)
        else {
            return formatting_response(id, Value::Null);
        };
        let client_options = crate::formatting::ClientOptions {
            tab_size: params.options.tab_size,
            insert_spaces: params.options.insert_spaces,
            trim_trailing_whitespace: params.options.trim_trailing_whitespace.unwrap_or(true),
            insert_final_newline: params.options.insert_final_newline.unwrap_or(true),
            trim_final_newlines: params.options.trim_final_newlines.unwrap_or(true),
        };
        // The document path drives `.editorconfig` resolution; a non-file document (e.g.
        // an untitled buffer) gets the client options alone instead of probing for an
        // `.editorconfig` relative to the server process's working directory.
        let document_path = crate::uri::file_uri_to_path(&params.text_document.uri);
        let Some(formatted) = crate::formatting::format_document(
            document_path.as_deref(),
            &open.text,
            &client_options,
        ) else {
            return formatting_response(id, Value::Null);
        };
        if formatted == open.text {
            return formatting_response(id, json!([]));
        }
        formatting_response(
            id,
            json!([{
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": byte_offset_to_position(&open.text, open.text.len()),
                },
                "newText": formatted,
            }]),
        )
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

    /// Dev-mode only: a single action whose command the client handles by navigating to the dump
    /// file the analysis thread wrote.
    ///
    /// The dump is produced on that thread, so the answer usually arrives later; the request id is
    /// parked here and resolved by [`LspService::complete_dump`].
    fn code_action(&mut self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        if !self.dev {
            return Dispatch::messages(vec![rpc_result(id, Value::Array(Vec::new()))]);
        }
        let Ok(params) = serde_json::from_value::<CodeActionParams>(params) else {
            return invalid_params(Some(id));
        };
        if self.pending_dumps.len() >= MAX_PENDING_DUMPS {
            return Dispatch::messages(vec![rpc_error(id, -32000, "too many pending dumps")]);
        }
        let token = self.next_dump_token;
        self.next_dump_token = self.next_dump_token.wrapping_add(1);
        let uri = params.text_document.uri;
        let position = params.range.start;
        match self.backend.dump(DumpJob {
            token,
            uri: uri.clone(),
        }) {
            Some(outcome) => {
                Dispatch::messages(vec![dump_code_actions(id, &uri, position, outcome.dump)])
            }
            None => {
                self.pending_dumps
                    .insert(token, PendingDump { id, uri, position });
                Dispatch::none()
            }
        }
    }

    fn complete_dump(&mut self, outcome: DumpOutcome) -> Option<Value> {
        let pending = self.pending_dumps.remove(&outcome.token)?;
        Some(dump_code_actions(
            pending.id,
            &pending.uri,
            pending.position,
            outcome.dump,
        ))
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

    fn document_waits_for_analysis(&self, uri: &str) -> bool {
        self.documents
            .get(uri)
            .is_some_and(|open| !open.analysis_blocked)
            && (self.analysis_dirty
                || self.analysis_in_flight
                || self.resubmit_pending
                || self.analysis_retry_at.is_some())
    }

    fn queue_exact_analysis_request(
        &mut self,
        id: Value,
        method: String,
        params: Value,
    ) -> Dispatch {
        let response_bytes = exact_analysis_response_bytes(&method);
        let pending_response_bytes = self
            .pending_analysis_requests
            .iter()
            .fold(0usize, |total, request| {
                total.saturating_add(request.exact_response_bytes())
            });
        if response_bytes > MAX_RETAINED_ANALYSIS_BYTES.saturating_sub(pending_response_bytes) {
            return Dispatch::messages(vec![rpc_error(
                id,
                -32802,
                "analysis request queue is full",
            )]);
        }
        let retained_bytes = retained_value_bytes(&id)
            .saturating_add(retained_value_bytes(&params))
            .saturating_add(std::mem::size_of::<PendingAnalysisRequest>())
            .saturating_add(method.capacity());
        if retained_bytes
            > MAX_PENDING_ANALYSIS_REQUEST_BYTES.saturating_sub(self.pending_analysis_request_bytes)
        {
            return Dispatch::messages(vec![rpc_error(
                id,
                -32802,
                "analysis request queue is full",
            )]);
        }
        self.pending_analysis_request_bytes = self
            .pending_analysis_request_bytes
            .saturating_add(retained_bytes);
        self.pending_analysis_requests
            .push_back(PendingAnalysisRequest {
                id,
                kind: PendingAnalysisRequestKind::Exact { method, params },
                retained_bytes,
            });
        Dispatch::none()
    }

    fn complete_pending_analysis_requests(&mut self) -> Vec<Value> {
        let pending = self.take_pending_analysis_requests_matching(|_| true);
        let mut messages = Vec::with_capacity(pending.len());
        for request in pending {
            match request.kind {
                PendingAnalysisRequestKind::Diagnostic {
                    uri,
                    previous_result_id,
                } => {
                    messages.push(diagnostic_report(
                        request.id,
                        self.documents.get(&uri).map(|open| &open.diagnostics),
                        previous_result_id.as_deref(),
                    ));
                }
                PendingAnalysisRequestKind::Exact { method, params } => {
                    messages.extend(
                        self.handle_inner(
                            json!({
                                "jsonrpc": "2.0",
                                "id": request.id,
                                "method": method,
                                "params": params,
                            }),
                            false,
                        )
                        .messages,
                    );
                }
            }
        }
        messages
    }

    fn cancel_pending_analysis_requests(
        &mut self,
        matches: impl FnMut(&PendingAnalysisRequest) -> bool,
        cancellation: PendingAnalysisCancellation,
    ) -> Vec<Value> {
        self.take_pending_analysis_requests_matching(matches)
            .into_iter()
            .map(|request| pending_analysis_cancellation(request, cancellation))
            .collect()
    }

    fn take_pending_analysis_requests_matching(
        &mut self,
        mut matches: impl FnMut(&PendingAnalysisRequest) -> bool,
    ) -> Vec<PendingAnalysisRequest> {
        let pending = std::mem::take(&mut self.pending_analysis_requests);
        let mut retained = VecDeque::with_capacity(pending.len());
        let mut matched = Vec::new();
        for request in pending {
            if matches(&request) {
                self.pending_analysis_request_bytes = self
                    .pending_analysis_request_bytes
                    .saturating_sub(request.retained_bytes);
                matched.push(request);
            } else {
                retained.push_back(request);
            }
        }
        self.pending_analysis_requests = retained;
        matched
    }

    fn cancel_request(&mut self, params: Value) -> Dispatch {
        let Some(id) = params.get("id").filter(|id| is_request_id(id)) else {
            return Dispatch::none();
        };
        let messages = self.cancel_pending_analysis_requests(
            |request| request.id.eq(id),
            PendingAnalysisCancellation::Client,
        );
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
        let previous_result_id = params.previous_result_id;
        if !self.document_waits_for_analysis(&uri) {
            // An open buffer is newer than whatever the sweep read from disk, so it always wins.
            if let Some(open) = self.documents.get(&uri) {
                return Dispatch::messages(vec![diagnostic_report(
                    id,
                    Some(&open.diagnostics),
                    previous_result_id.as_deref(),
                )]);
            }
            let indexed = self
                .workspace_diagnostics
                .diagnostics(&uri)
                .map(|found| DiagnosticIndex::from_workspace(&found));
            return Dispatch::messages(vec![diagnostic_report(
                id,
                indexed.as_ref(),
                previous_result_id.as_deref(),
            )]);
        }

        let retained_bytes = retained_value_bytes(&id)
            .saturating_add(std::mem::size_of::<PendingAnalysisRequest>())
            .saturating_add(uri.capacity())
            .saturating_add(previous_result_id.as_ref().map_or(0, String::capacity));
        let replaced = self.pending_analysis_requests.iter().find(|request| {
            matches!(
                &request.kind,
                PendingAnalysisRequestKind::Diagnostic {
                    uri: request_uri, ..
                } if request_uri == &uri
            )
        });
        let current_bytes = self.pending_analysis_request_bytes.saturating_sub(
            replaced
                .map(|request| request.retained_bytes)
                .unwrap_or_default(),
        );
        if retained_bytes > MAX_PENDING_ANALYSIS_REQUEST_BYTES.saturating_sub(current_bytes) {
            return Dispatch::messages(vec![diagnostic_server_cancelled(
                id,
                "diagnostic analysis request queue is full",
                true,
            )]);
        }
        let messages = self
            .take_pending_analysis_requests_matching(|request| {
                matches!(
                    &request.kind,
                    PendingAnalysisRequestKind::Diagnostic {
                        uri: request_uri, ..
                    } if request_uri == &uri
                )
            })
            .into_iter()
            .map(|request| {
                diagnostic_server_cancelled(
                    request.id,
                    "diagnostic request was superseded by a newer pull",
                    false,
                )
            })
            .collect();
        self.pending_analysis_request_bytes = self
            .pending_analysis_request_bytes
            .saturating_add(retained_bytes);
        self.pending_analysis_requests
            .push_back(PendingAnalysisRequest {
                id,
                kind: PendingAnalysisRequestKind::Diagnostic {
                    uri,
                    previous_result_id,
                },
                retained_bytes,
            });
        Dispatch::messages(messages)
    }

    /// Report every file the sweep has indexed. Without this the retained diagnostics are
    /// unreachable: a client only pulls `textDocument/diagnostic` for documents it has open, and
    /// those are always answered from the open buffer. To avoid showing the same diagnostic twice
    /// in editors such as Zed, open documents are reported as empty here when the client will also
    /// pull them through `textDocument/diagnostic`.
    fn workspace_diagnostic(&mut self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<WorkspaceDiagnosticParams>(params) else {
            return invalid_params(Some(id));
        };
        if params.previous_result_ids.len() > MAX_WORKSPACE_DIAGNOSTIC_REPORTS {
            return Dispatch::messages(vec![diagnostic_server_cancelled(
                id,
                "workspace diagnostic prior-result set exceeds the response limit",
                false,
            )]);
        }
        let previous: HashMap<String, String> = params
            .previous_result_ids
            .into_iter()
            .map(|previous| (previous.uri, previous.value))
            .collect();
        // Include prior client state even when the file disappeared: omitting that URI would leave
        // its old diagnostics installed forever. Open buffers also belong here and are authoritative
        // over the sweep, just as they are for textDocument/diagnostic.
        let mut uris = self.workspace_diagnostics.indexed_uris();
        uris.extend(self.documents.keys().cloned());
        uris.extend(previous.keys().cloned());
        uris.sort_unstable();
        uris.dedup();
        if uris.len() > MAX_WORKSPACE_DIAGNOSTIC_REPORTS {
            return Dispatch::messages(vec![diagnostic_server_cancelled(
                id,
                "workspace diagnostic report exceeds the bounded non-streaming response limit",
                false,
            )]);
        }

        let mut items = Vec::new();
        let mut item_wire_bytes = 2usize;
        for uri in uris {
            let workspace_index;
            let open_empty_index;
            let empty_index;
            let index = if let Some(open) = self.documents.get(&uri) {
                if self.client_pulls_diagnostics {
                    // The client pulls open documents via textDocument/diagnostic. Returning them
                    // again in the workspace report makes editors such as Zed display every
                    // diagnostic twice, so the workspace report clears its copy instead.
                    open_empty_index = DiagnosticIndex::default();
                    &open_empty_index
                } else {
                    &open.diagnostics
                }
            } else if let Some(found) = self.workspace_diagnostics.diagnostics(&uri) {
                workspace_index = DiagnosticIndex::from_workspace(&found);
                &workspace_index
            } else {
                empty_index = DiagnosticIndex::default();
                &empty_index
            };
            let result_id = index.result_id();
            let item = if previous.get(&uri).map(String::as_str) == Some(result_id.as_str()) {
                json!({
                    "kind": "unchanged",
                    "uri": uri,
                    "version": Value::Null,
                    "resultId": result_id,
                })
            } else {
                json!({
                "kind": "full",
                "uri": uri,
                    "version": Value::Null,
                    "resultId": result_id,
                "items": index.encode(),
                })
            };
            let Ok(encoded) = serde_json::to_vec(&item) else {
                return Dispatch::messages(vec![rpc_error(
                    id,
                    -32603,
                    "workspace diagnostic serialization failed",
                )]);
            };
            item_wire_bytes = item_wire_bytes
                .saturating_add(encoded.len())
                .saturating_add(1);
            if item_wire_bytes > BOUNDED_EXACT_RESPONSE_BYTES {
                return Dispatch::messages(vec![diagnostic_server_cancelled(
                    id,
                    "workspace diagnostic report exceeds the bounded non-streaming response limit",
                    false,
                )]);
            }
            items.push(item);
        }
        let response = rpc_result(id.clone(), json!({"items": items}));
        if !serialized_value_fits(&response, MAX_MESSAGE_BYTES) {
            return Dispatch::messages(vec![diagnostic_server_cancelled(
                id,
                "workspace diagnostic response exceeds the protocol message limit",
                false,
            )]);
        }
        Dispatch::messages(vec![response])
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

const DUMP_ACTION_TITLE: &str = "krusty (dev): dump AST + checker + IR";

/// The command the dump action carries. The editor handles it itself — it never reaches this server
/// as `workspace/executeCommand` — but it must still be advertised, see [`DUMP_ACTION_COMMANDS`].
const DUMP_ACTION_COMMAND: &str = "editor.action.goToLocations";

/// Commands advertised in `executeCommandProvider` under `--dev`.
///
/// Advertising is not optional even though the editor never sends the command back. Zed drops any
/// returned code action whose `command` is absent from this list, before the action reaches the
/// menu and without reporting anything: the user sees "no code actions available" from a server
/// that answered correctly. (`crates/project/src/lsp_command.rs`, where `available_commands` comes
/// from the server's own `execute_command_provider` and defaults to empty.)
const DUMP_ACTION_COMMANDS: [&str; 1] = [DUMP_ACTION_COMMAND];

/// The code-action response for a finished dump: one navigation action, or an empty list when the
/// document had no dump to open.
///
/// The `arguments` array must carry three elements. Zed reads `arguments[2]` as the location list
/// and ignores the first two, but returns early when fewer than three are present — which turns the
/// action into a silent no-op with no error anywhere.
fn dump_code_actions(id: Value, uri: &str, position: Position, dump: Option<DumpResult>) -> Value {
    let Some(dump_uri) = dump.and_then(|dump| path_to_file_uri(&dump.path)) else {
        return rpc_result(id, Value::Array(Vec::new()));
    };
    let zero = json!({"line": 0, "character": 0});
    let location = json!({"uri": dump_uri, "range": {"start": zero, "end": zero}});
    rpc_result(
        id,
        json!([{
            "title": DUMP_ACTION_TITLE,
            "kind": "source",
            "command": {
                "title": DUMP_ACTION_TITLE,
                "command": DUMP_ACTION_COMMAND,
                "arguments": [uri, position, [location]],
            }
        }]),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentSymbolParams {
    text_document: TextDocumentIdentifier,
}

#[derive(Deserialize)]
struct WorkspaceSymbolParams {
    query: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentFormattingParams {
    text_document: TextDocumentIdentifier,
    options: FormattingOptions,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FormattingOptions {
    tab_size: u32,
    insert_spaces: bool,
    // Optional per LSP; absent means "let the server decide", which for us is the
    // ktlint default (true), not the serde false default.
    #[serde(default)]
    trim_trailing_whitespace: Option<bool>,
    #[serde(default)]
    insert_final_newline: Option<bool>,
    #[serde(default)]
    trim_final_newlines: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentDiagnosticParams {
    text_document: TextDocumentIdentifier,
    #[serde(default)]
    previous_result_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceDiagnosticParams {
    #[serde(default)]
    previous_result_ids: Vec<WorkspacePreviousResultId>,
}

#[derive(Deserialize)]
struct WorkspacePreviousResultId {
    uri: String,
    value: String,
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
struct CodeActionParams {
    text_document: TextDocumentIdentifier,
    range: Range,
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

struct BoundedJsonWriter {
    written: usize,
    limit: usize,
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.written) {
            return Err(io::Error::other("JSON response exceeds byte limit"));
        }
        self.written += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_value_fits(value: &Value, limit: usize) -> bool {
    serde_json::to_writer(BoundedJsonWriter { written: 0, limit }, value).is_ok()
}

fn formatting_rpc_result_with_limits(
    id: Value,
    result: Value,
    result_limit: usize,
    response_limit: usize,
) -> Value {
    let mut response = rpc_result(id, result);
    let result_fits = response
        .get("result")
        .is_some_and(|result| serialized_value_fits(result, result_limit));
    if result_fits && serialized_value_fits(&response, response_limit) {
        return response;
    }
    response
        .as_object_mut()
        .expect("the RPC response envelope is an object")
        .insert("result".to_string(), Value::Null);
    debug_assert!(serialized_value_fits(&response, response_limit));
    response
}

fn formatting_response(id: Value, result: Value) -> Dispatch {
    Dispatch::messages(vec![formatting_rpc_result_with_limits(
        id,
        result,
        MAX_FORMATTING_RESULT_BYTES,
        MAX_MESSAGE_BYTES,
    )])
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

fn client_supports_diagnostic_refresh(params: &Value) -> bool {
    params
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("workspace"))
        .and_then(|workspace| workspace.get("diagnostics"))
        .and_then(|diagnostics| diagnostics.get("refreshSupport"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn client_supports_pull_diagnostics(params: &Value) -> bool {
    params
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("textDocument"))
        .and_then(|text_document| text_document.get("diagnostic"))
        .is_some_and(|diagnostic| !diagnostic.is_null())
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

fn pending_analysis_cancellation(
    request: PendingAnalysisRequest,
    cancellation: PendingAnalysisCancellation,
) -> Value {
    let diagnostic = matches!(&request.kind, PendingAnalysisRequestKind::Diagnostic { .. });
    match (diagnostic, cancellation) {
        (_, PendingAnalysisCancellation::Client) => {
            rpc_error(request.id, -32800, "request cancelled")
        }
        (true, PendingAnalysisCancellation::DocumentChanged) => diagnostic_server_cancelled(
            request.id,
            "diagnostic request was cancelled because the document changed",
            true,
        ),
        (true, PendingAnalysisCancellation::DocumentClosed) => {
            diagnostic_report(request.id, None, None)
        }
        (true, PendingAnalysisCancellation::Shutdown) => diagnostic_server_cancelled(
            request.id,
            "diagnostic request was cancelled because the server is shutting down",
            false,
        ),
        (false, PendingAnalysisCancellation::DocumentChanged) => rpc_error(
            request.id,
            -32801,
            "analysis request was cancelled because the document changed",
        ),
        (false, PendingAnalysisCancellation::DocumentClosed) => rpc_error(
            request.id,
            -32801,
            "analysis request was cancelled because the document was closed",
        ),
        (false, PendingAnalysisCancellation::Shutdown) => rpc_error(
            request.id,
            -32800,
            "analysis request was cancelled because the server is shutting down",
        ),
    }
}

fn is_request_id(id: &Value) -> bool {
    match id {
        Value::Number(id) => id.is_i64() || id.is_u64(),
        Value::String(_) => true,
        _ => false,
    }
}

fn diagnostic_report(
    id: Value,
    diagnostics: Option<&DiagnosticIndex>,
    previous_result_id: Option<&str>,
) -> Value {
    let result_id = diagnostics
        .map(DiagnosticIndex::result_id)
        .unwrap_or_else(DiagnosticIndex::empty_result_id);
    if previous_result_id == Some(result_id.as_str()) {
        return rpc_result(id, json!({"kind": "unchanged", "resultId": result_id}));
    }
    let items = diagnostics.map(DiagnosticIndex::encode).unwrap_or_default();
    rpc_result(
        id,
        json!({"kind": "full", "resultId": result_id, "items": items}),
    )
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
pub(super) fn lsp_diagnostic_message(mut message: String) -> String {
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
            identity: None,
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

fn exact_analysis_request_uri<'a>(method: &str, params: &'a Value) -> Option<&'a str> {
    if !matches!(
        method,
        "textDocument/definition"
            | "textDocument/typeDefinition"
            | "textDocument/implementation"
            | "textDocument/references"
            | "textDocument/rename"
            | "textDocument/documentSymbol"
            | "textDocument/codeAction"
            | "textDocument/foldingRange"
            | "textDocument/semanticTokens/full"
            | "textDocument/semanticTokens/range"
    ) {
        return None;
    }
    params.pointer("/textDocument/uri").and_then(Value::as_str)
}

fn exact_analysis_response_bytes(method: &str) -> usize {
    match method {
        "textDocument/rename"
        | "textDocument/documentSymbol"
        | "textDocument/codeAction"
        | "textDocument/foldingRange"
        | "workspace/symbol" => BOUNDED_EXACT_RESPONSE_BYTES,
        _ => MAX_RETAINED_ANALYSIS_BYTES,
    }
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
        EngineEvent::IndexProgress(batch) => {
            for message in service.apply_index_batch(batch) {
                let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                write_framed(writer, &encoded)?;
            }
        }
        EngineEvent::DependencyIndex { generation, index } => {
            service.set_dependency_index(generation, index);
        }
        EngineEvent::DependenciesLocated {
            generation,
            attempted,
            located,
        } => {
            service.record_located_dependencies(generation, attempted, located);
        }
        EngineEvent::SymbolIndexProgress(batch) => {
            for message in service.apply_symbol_index_batch(*batch) {
                let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                write_framed(writer, &encoded)?;
            }
        }
        EngineEvent::IndexReset(generation) => {
            for message in service.reset_workspace_index(generation) {
                let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                write_framed(writer, &encoded)?;
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
        EngineEvent::Dumped(outcome) => {
            if let Some(message) = service.complete_dump(outcome) {
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

/// `dev` turns on the developer surfaces; it must be the same flag the analysis host was built
/// with, because the capability is advertised here while the dump is produced there.
pub fn run_stdio_connection_async<A>(analyze: A, dev: bool) -> io::Result<i32>
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
    let service = LspService::with_backend(backend).with_dev(dev);
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
    let _ = shutdown_engine(
        service.backend.into_engine(),
        &incoming,
        ENGINE_SHUTDOWN_GRACE,
    );
    outcome
}

/// Disconnect the analysis command queue and wait at most `grace` for its thread to unwind.
///
/// Returning whether the thread joined makes the exceptional detach path directly testable without
/// baking the production two-second budget into a unit test. While waiting, drain engine events: the
/// engine uses a bounded event channel and could otherwise be finished with analysis but blocked trying
/// to publish its last result. Once the deadline expires, dropping the join handle deliberately detaches
/// the thread so the server's main thread can return and process teardown can reclaim it.
fn shutdown_engine(
    mut engine: AnalysisEngine,
    incoming: &Receiver<Incoming>,
    grace: Duration,
) -> bool {
    engine.disconnect();
    let deadline = Instant::now()
        .checked_add(grace)
        .unwrap_or_else(Instant::now);
    while !engine.is_finished() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        // Do not overshoot a short test budget by the production polling interval.
        let _ = incoming.recv_timeout(remaining.min(Duration::from_millis(50)));
    }
    if engine.is_finished() {
        engine.join();
        true
    } else {
        // The analysis thread is wedged. Leave it detached and let process teardown
        // reap it; blocking here would strand the server after the client is gone.
        engine.abandon();
        false
    }
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

    #[test]
    fn disk_symbol_index_reports_a_file_rejected_before_reading() {
        struct RemoveOnDrop(std::path::PathBuf);
        impl Drop for RemoveOnDrop {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        let path = std::env::temp_dir().join(format!(
            "krusty-lsp-oversized-symbol-index-{}.kt",
            std::process::id()
        ));
        let cleanup = RemoveOnDrop(path.clone());
        let file = std::fs::File::create(&path).expect("create sparse oversized source");
        file.set_len(crate::analysis::MAX_INDEXED_FILE_BYTES as u64 + 1)
            .expect("size sparse oversized source");
        drop(file);
        let uri = crate::uri::path_to_file_uri(&path).expect("temporary path is a file URI");

        let index = index_workspace_symbols_from_disk(&[&uri]);

        assert_eq!(index.entry_count(), 0);
        assert!(
            !index.is_complete(),
            "pre-read size rejection must reach the retained index completeness flag"
        );
        // The skip keeps its provenance: the URI and size are what the client log reports.
        assert_eq!(index.omissions().oversized_files, 1);
        assert_eq!(
            index.omissions().oversized_examples,
            vec![(
                uri.clone(),
                crate::analysis::MAX_INDEXED_FILE_BYTES as u64 + 1
            )]
        );
        drop(cleanup);
    }

    #[test]
    fn formatting_response_bounds_cover_the_complete_rpc_envelope() {
        let id = Value::String("request-with-escapes-\"-\\".to_string());
        let unchanged = rpc_result(id.clone(), json!([]));
        let unchanged_bytes = serde_json::to_vec(&unchanged).unwrap().len();
        assert_eq!(
            formatting_rpc_result_with_limits(id.clone(), json!([]), 2, unchanged_bytes),
            unchanged
        );

        let edit = json!([{
            "range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 1}
            },
            "newText": "formatted"
        }]);
        let edit_response = rpc_result(id.clone(), edit.clone());
        let edit_result_bytes = serde_json::to_vec(&edit).unwrap().len();
        let edit_bytes = serde_json::to_vec(&edit_response).unwrap().len();
        assert_eq!(
            formatting_rpc_result_with_limits(
                id.clone(),
                edit.clone(),
                edit_result_bytes,
                edit_bytes
            ),
            edit_response
        );

        let null_response = rpc_result(id.clone(), Value::Null);
        let null_bytes = serde_json::to_vec(&null_response).unwrap().len();
        assert_eq!(
            formatting_rpc_result_with_limits(
                id.clone(),
                edit.clone(),
                edit_result_bytes - 1,
                null_bytes
            ),
            null_response
        );
        let envelope_fallback =
            formatting_rpc_result_with_limits(id.clone(), edit, edit_result_bytes, null_bytes);
        assert_eq!(envelope_fallback["id"], id);
        assert_eq!(envelope_fallback["result"], Value::Null);
        assert_eq!(
            serde_json::to_vec(&envelope_fallback).unwrap().len(),
            null_bytes
        );
        assert_eq!(
            formatting_rpc_result_with_limits(
                id.clone(),
                json!([{"newText": "larger than the bounded response"}]),
                1,
                null_bytes
            ),
            rpc_result(id, Value::Null)
        );
    }

    fn decode_messages(bytes: &[u8]) -> Vec<Value> {
        let mut reader = io::Cursor::new(bytes);
        let mut messages = Vec::new();
        while let Some(body) = read_framed(&mut reader, MAX_MESSAGE_BYTES).unwrap() {
            messages.push(serde_json::from_slice(&body).unwrap());
        }
        messages
    }

    fn analysis_with_diagnostic(message: &str) -> DocumentAnalysis {
        DocumentAnalysis::with_diagnostics(vec![Diagnostic {
            span: krusty::diag::Span::new(0, 1),
            editor_span: None,
            identity: None,
            severity: Severity::Error,
            kind: DiagnosticKind::Compiler,
            msg: message.to_string(),
            file: 0,
        }])
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

    type DependencyRequests = std::rc::Rc<std::cell::RefCell<Vec<(u64, Vec<String>)>>>;

    struct DependencyRecordingBackend {
        requested: DependencyRequests,
    }

    impl AnalysisBackend for DependencyRecordingBackend {
        fn analysis_ready(&self) -> bool {
            true
        }

        fn submit(&mut self, _job: AnalysisJob) -> Option<AnalysisBatch> {
            None
        }

        fn locate_dependencies(&mut self, generation: u64, candidates: Vec<DependencyCandidate>) {
            self.requested.borrow_mut().push((
                generation,
                candidates
                    .into_iter()
                    .map(|candidate| candidate.internal)
                    .collect(),
            ));
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

        let messages = decode_messages(&out);
        let publish = messages
            .iter()
            .find(|message| message["method"] == "textDocument/publishDiagnostics")
            .expect("analysis result must be published");
        assert_eq!(publish["params"]["uri"], "file:///a.kt");
        assert_eq!(
            publish["params"]["diagnostics"].as_array().map(Vec::len),
            Some(0)
        );
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

    fn dump_code_action_request(id: i64, uri: &str, line: u32, character: u32) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/codeAction",
            "params": {
                "textDocument": {"uri": uri},
                "range": {
                    "start": {"line": line, "character": character},
                    "end": {"line": line, "character": character}
                },
                "context": {"diagnostics": []}
            }
        })
    }

    fn drain_engine(engine: AnalysisEngine, incoming: &Receiver<Incoming>) {
        let mut engine = engine;
        engine.disconnect();
        while !engine.is_finished() {
            let _ = incoming.recv_timeout(Duration::from_millis(50));
        }
        engine.join();
    }

    /// The production server runs on `EngineBackend`, which answers a dump only after the analysis
    /// thread has written it. A code action that works solely through an inline stub would be dead
    /// in the real editor, so the whole deferred round trip is exercised here.
    #[test]
    fn dev_mode_code_action_is_answered_through_the_engine_backend() {
        use std::sync::mpsc::sync_channel;

        struct DumpHost;
        impl Analysis for DumpHost {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }

            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
            fn dump(&mut self, uri: &str) -> Option<DumpResult> {
                (uri == "file:///w/Main.kt").then(|| DumpResult {
                    path: PathBuf::from("/cache/dumps/Main.kt.krusty.md"),
                })
            }
        }

        let (sender, incoming) = sync_channel::<Incoming>(INPUT_QUEUE_CAPACITY);
        let engine = AnalysisEngine::spawn(DumpHost, sender.clone());
        let mut service = LspService::with_backend(EngineBackend::new(engine, true)).with_dev(true);
        service.force_initialized_for_test();

        let mut pending = VecDeque::new();
        let mut out: Vec<u8> = Vec::new();

        let ready = incoming.recv().expect("engine startup ready-state");
        step_async(&mut service, &mut out, &incoming, &mut pending, ready).unwrap();

        step_async(
            &mut service,
            &mut out,
            &incoming,
            &mut pending,
            Incoming::Message(dump_code_action_request(7, "file:///w/Main.kt", 2, 1)),
        )
        .unwrap();
        assert!(
            decode_messages(&out)
                .iter()
                .all(|message| message["id"] != 7),
            "the engine backend must answer the dump asynchronously"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        let response = loop {
            if let Some(message) = decode_messages(&out)
                .into_iter()
                .find(|message| message["id"] == 7)
            {
                break message;
            }
            assert!(Instant::now() < deadline, "no dump response arrived");
            let event = match pending.pop_front() {
                Some(event) => Some(event),
                None => incoming.recv_timeout(Duration::from_millis(100)).ok(),
            };
            if let Some(event) = event {
                step_async(&mut service, &mut out, &incoming, &mut pending, event).unwrap();
            }
        };

        let actions = response["result"].as_array().expect("array result");
        assert_eq!(actions.len(), 1, "{response}");
        assert_eq!(actions[0]["kind"], "source");
        let command = &actions[0]["command"];
        assert_eq!(command["command"], "editor.action.goToLocations");
        let arguments = command["arguments"].as_array().expect("arguments array");
        assert_eq!(
            arguments.len(),
            3,
            "Zed drops the command when arguments.len() < 3"
        );
        assert_eq!(arguments[0], "file:///w/Main.kt");
        assert_eq!(arguments[1], json!({"line": 2, "character": 1}));
        assert_eq!(
            arguments[2][0]["uri"],
            "file:///cache/dumps/Main.kt.krusty.md"
        );

        drain_engine(service.backend.into_engine(), &incoming);
    }

    /// Holding down the keybinding must not grow the pending map without bound.
    #[test]
    fn pending_dumps_are_bounded() {
        use std::sync::mpsc::sync_channel;

        struct Mock;
        impl Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }

            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
        }

        let (sender, incoming) = sync_channel::<Incoming>(INPUT_QUEUE_CAPACITY);
        let engine = AnalysisEngine::spawn(Mock, sender.clone());
        let mut service = LspService::with_backend(EngineBackend::new(engine, true)).with_dev(true);
        service.force_initialized_for_test();

        for id in 0..MAX_PENDING_DUMPS {
            let dispatch = service.handle(dump_code_action_request(
                id as i64,
                "file:///w/Main.kt",
                0,
                0,
            ));
            assert!(
                dispatch.messages.is_empty(),
                "request {id} must wait for the analysis thread"
            );
        }
        assert_eq!(service.pending_dumps.len(), MAX_PENDING_DUMPS);

        let dispatch = service.handle(dump_code_action_request(
            MAX_PENDING_DUMPS as i64,
            "file:///w/Main.kt",
            0,
            0,
        ));
        assert_eq!(dispatch.messages.len(), 1);
        assert_eq!(dispatch.messages[0]["error"]["code"], -32000);
        assert_eq!(
            service.pending_dumps.len(),
            MAX_PENDING_DUMPS,
            "a rejected dump must not be retained"
        );

        drain_engine(service.backend.into_engine(), &incoming);
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
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
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
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
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
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
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
    fn shutdown_abandons_analysis_that_exceeds_its_grace_period() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc::{channel, sync_channel};
        use std::sync::Arc;

        struct BlockingAnalysis {
            entered: std::sync::mpsc::Sender<()>,
            release: std::sync::mpsc::Receiver<()>,
            completed: Arc<AtomicBool>,
        }
        impl Analysis for BlockingAnalysis {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                // This fixture isolates shutdown while an interactive analysis is blocked; it has
                // no project model and therefore no legitimate workspace-index producer.
                IndexOutcome::default()
            }

            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                let _ = self.entered.send(());
                let _ = self.release.recv();
                self.completed.store(true, Ordering::SeqCst);
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
        }

        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        let completed = Arc::new(AtomicBool::new(false));
        let analysis = BlockingAnalysis {
            entered: entered_tx,
            release: release_rx,
            completed: completed.clone(),
        };
        let (events, incoming) = sync_channel(INPUT_QUEUE_CAPACITY);
        let engine = AnalysisEngine::spawn(analysis, events);
        engine.submit(EngineCommand::Analyze(AnalysisJob {
            documents: vec![("file:///blocked.kt".into(), "fun blocked() {}".into(), 1)],
            open_uris: vec!["file:///blocked.kt".into()],
        }));
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("analysis entered its blocking section");

        let started = Instant::now();
        assert!(
            !shutdown_engine(engine, &incoming, Duration::from_millis(20)),
            "a still-running analysis must take the detach path"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "shutdown exceeded its bounded grace period"
        );

        // Detachment must not corrupt the engine's own unwind path. Release the synthetic block and
        // wait for it to finish before the test drops its channels, keeping this regression leak-free.
        release_tx.send(()).expect("release detached analysis");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !completed.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            completed.load(Ordering::SeqCst),
            "detached analysis did not unwind after its blocker was released"
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
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
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

        let answered = decode_messages(&out[before..]);
        assert!(
            answered
                .iter()
                .any(|message| message.get("id") == Some(&json!(2))),
            "hover response must be written while analysis is in flight: {answered:?}"
        );
        assert!(
            !completed.load(Ordering::SeqCst),
            "the hover was answered before analysis completed"
        );

        let before_release = out.len();
        release_tx.send(()).unwrap();
        let mut after_release = Vec::new();
        for _ in 0..INPUT_QUEUE_CAPACITY + 2 {
            let event = incoming
                .recv_timeout(Duration::from_secs(5))
                .expect("analysis completion event");
            step_async(&mut service, &mut out, &incoming, &mut pending, event).unwrap();
            after_release = decode_messages(&out[before_release..]);
            if after_release
                .iter()
                .any(|message| message["method"] == "textDocument/publishDiagnostics")
            {
                break;
            }
        }
        assert!(
            after_release
                .iter()
                .any(|message| message["method"] == "textDocument/publishDiagnostics"),
            "analysis result is published after release"
        );
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
                service.pending_analysis_requests.len(),
                1,
                "duplicate pulls must never retain duplicate full reports"
            );
        }
        assert!(service.pending_analysis_request_bytes <= MAX_PENDING_ANALYSIS_REQUEST_BYTES);

        let mut spare_capacity_id = String::with_capacity(MAX_PENDING_ANALYSIS_REQUEST_BYTES + 1);
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
        assert_eq!(service.pending_analysis_requests.len(), 1);
        assert!(service.pending_analysis_request_bytes > 0);

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
            service.pending_analysis_requests.len(),
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
        assert_eq!(service.pending_analysis_requests.len(), 1);

        let maximum_message = "x".repeat(MAX_SOURCE_SET_DIAGNOSTIC_TEXT_BYTES);
        let batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis::with_diagnostics(vec![Diagnostic {
                span: krusty::diag::Span::new(0, 3),
                editor_span: None,
                identity: None,
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
        assert!(service.pending_analysis_requests.is_empty());
        assert_eq!(service.pending_analysis_request_bytes, 0);
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
        assert!(service.pending_analysis_requests.is_empty());
        assert_eq!(service.pending_analysis_request_bytes, 0);
    }

    #[test]
    fn semantic_tokens_wait_for_the_current_analysis_batch() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        let uri = "file:///document.kt";
        let source = "fun value() = 1\n";
        service.open_document_for_test(uri, source, 1);
        service.mark_analysis_dirty_for_test();
        let _job = service
            .dispatch_pending_analysis()
            .expect("analysis starts for the open document");

        let pending = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "tokens",
            "method": "textDocument/semanticTokens/full",
            "params": {"textDocument": {"uri": uri}}
        }));
        assert!(pending.messages.is_empty());

        let messages = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![(uri.into(), 1)],
            analyses: crate::analysis::analyze_for_lsp(&[source]),
            support_documents: Vec::new(),
            pending: false,
        });
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["id"], "tokens");
        assert!(messages[1]["result"]["data"]
            .as_array()
            .is_some_and(|data| !data.is_empty()));
    }

    #[test]
    fn workspace_symbols_wait_for_analysis_and_include_unopened_support_sources() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        let open_uri = "file:///Open.kt";
        let support_uri = "file:///Support.kt";
        let open_source = "package demo\nclass Open\n";
        let support_source = "package demo\nclass UnopenedSupport\n";
        service.open_document_for_test(open_uri, open_source, 1);
        service.mark_analysis_dirty_for_test();
        let _job = service
            .dispatch_pending_analysis()
            .expect("analysis starts for the open document");

        let pending = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "workspace-1",
            "method": "workspace/symbol",
            "params": {"query": "UnopenedSupport"}
        }));
        assert!(
            pending.messages.is_empty(),
            "workspace symbols must not observe the empty pre-analysis snapshot"
        );

        let mut analyses = crate::analysis::analyze_for_lsp(&[open_source, support_source]);
        analyses.truncate(1);
        let messages = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![(open_uri.into(), 1)],
            analyses,
            support_documents: vec![(support_uri.into(), support_source.into())],
            pending: false,
        });

        assert_eq!(messages.len(), 2, "one publish plus one symbol response");
        assert_eq!(
            messages[1]["result"],
            json!([{
                "name": "UnopenedSupport",
                "kind": 5,
                "containerName": "demo",
                "location": {
                    "uri": support_uri,
                    "range": {
                        "start": {"line": 1, "character": 6},
                        "end": {"line": 1, "character": 21},
                    },
                },
            }])
        );
    }

    #[test]
    fn dependency_symbols_never_block_a_query_and_arrive_on_the_next_one() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        service.set_dependency_index_for_test(crate::DependencySymbolIndex::from_internal_names([
            "kotlin/collections/AbstractList".to_string(),
        ]));

        // Nothing is located yet, so the first query answers without it rather than waiting on a
        // render. The picker asks again on the next keystroke.
        let first = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "dep-first",
            "method": "workspace/symbol",
            "params": {"query": "AbstractList"}
        }));
        assert_eq!(first.messages[0]["result"], json!([]));

        service.record_located_dependencies_for_test(vec![crate::LocatedDependency {
            candidate: crate::DependencyCandidate {
                internal: "kotlin/collections/AbstractList".to_string(),
                package: "kotlin.collections".to_string(),
                name: "AbstractList".to_string(),
            },
            path: std::path::PathBuf::from("/cache/kotlin/collections/AbstractList.kt"),
            // `AbstractList` begins 6 bytes into the third line.
            span: krusty::diag::Span::new(34, 46),
            text: "package kotlin.collections\n\nclass AbstractList\n".to_string(),
        }]);

        let second = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "dep-second",
            "method": "workspace/symbol",
            "params": {"query": "AbstractList"}
        }));
        let symbol = &second.messages[0]["result"][0];
        assert_eq!(symbol["name"], "AbstractList");
        assert_eq!(symbol["containerName"], "kotlin.collections");
        // A range the client can open, resolved from the byte span in the written text.
        assert_eq!(symbol["location"]["range"]["start"]["line"], 2);
        assert_eq!(symbol["location"]["range"]["start"]["character"], 6);
        assert_eq!(symbol["location"]["range"]["end"]["character"], 18);
    }

    #[test]
    fn a_failed_dependency_location_is_released_for_retry() {
        let requested = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(DependencyRecordingBackend {
            requested: requested.clone(),
        });
        service.force_initialized_for_test();
        service.set_dependency_index_for_test(crate::DependencySymbolIndex::from_internal_names([
            "vendor/Retryable".to_string(),
        ]));

        let query = || {
            json!({
                "jsonrpc": "2.0",
                "id": "retryable",
                "method": "workspace/symbol",
                "params": {"query": "Retryable"}
            })
        };
        assert_eq!(service.handle(query()).messages[0]["result"], json!([]));
        assert_eq!(requested.borrow().len(), 1);

        let attempted = requested.borrow()[0].1.clone();
        service.record_located_dependencies(0, attempted, Vec::new());

        assert_eq!(service.handle(query()).messages[0]["result"], json!([]));
        assert_eq!(
            requested.borrow().len(),
            2,
            "a completed failure is no longer in flight and must be eligible for the next attempt"
        );
    }

    #[test]
    fn stale_dependency_results_cannot_cross_a_project_generation() {
        let requested = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(DependencyRecordingBackend {
            requested: requested.clone(),
        });
        service.force_initialized_for_test();
        service.reset_workspace_index(1);
        service.set_dependency_index(
            1,
            crate::DependencySymbolIndex::from_internal_names(["vendor/Current".to_string()]),
        );

        // This completed after the reset but belongs to generation zero. Accepting it would make a
        // file rendered from the previous classpath visible under the replacement model.
        service.record_located_dependencies(
            0,
            vec!["vendor/Current".to_string()],
            vec![crate::LocatedDependency {
                candidate: crate::DependencyCandidate {
                    internal: "vendor/Current".to_string(),
                    package: "vendor".to_string(),
                    name: "Current".to_string(),
                },
                path: std::path::PathBuf::from("/cache/old/vendor/Current.kt"),
                span: krusty::diag::Span::new(22, 29),
                text: "package vendor\n\nclass Current\n".to_string(),
            }],
        );

        let answered = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "current",
            "method": "workspace/symbol",
            "params": {"query": "Current"}
        }));
        assert_eq!(answered.messages[0]["result"], json!([]));
        assert_eq!(
            requested.borrow().as_slice(),
            &[(1, vec!["vendor/Current".to_string()])]
        );
    }

    #[test]
    fn a_new_classpath_forgets_what_the_old_one_rendered() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        service.set_dependency_index_for_test(crate::DependencySymbolIndex::from_internal_names([
            "vendor/Versioned".to_string(),
        ]));
        service.record_located_dependencies_for_test(vec![crate::LocatedDependency {
            candidate: crate::DependencyCandidate {
                internal: "vendor/Versioned".to_string(),
                package: "vendor".to_string(),
                name: "Versioned".to_string(),
            },
            path: std::path::PathBuf::from("/cache/old/vendor/Versioned.kt"),
            span: krusty::diag::Span::new(22, 31),
            text: "package vendor\n\nclass Versioned\n".to_string(),
        }]);

        // A dependency version bump replaces the index. The rendered source from the old version is
        // still on disk -- the cache is content-addressed -- and would otherwise be served forever.
        service.set_dependency_index_for_test(crate::DependencySymbolIndex::from_internal_names([
            "vendor/Versioned".to_string(),
        ]));

        let answered = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "versioned",
            "method": "workspace/symbol",
            "params": {"query": "Versioned"}
        }));
        assert_eq!(
            answered.messages[0]["result"],
            json!([]),
            "the old version's source must not answer for the new one"
        );
    }

    #[test]
    fn a_qualified_query_gets_a_qualified_dependency_name() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        service.set_dependency_index_for_test(crate::DependencySymbolIndex::from_internal_names([
            "kotlin/collections/AbstractList".to_string(),
        ]));
        service.record_located_dependencies_for_test(vec![crate::LocatedDependency {
            candidate: crate::DependencyCandidate {
                internal: "kotlin/collections/AbstractList".to_string(),
                package: "kotlin.collections".to_string(),
                name: "AbstractList".to_string(),
            },
            path: std::path::PathBuf::from("/cache/kotlin/collections/AbstractList.kt"),
            span: krusty::diag::Span::new(34, 46),
            text: "package kotlin.collections\n\nclass AbstractList\n".to_string(),
        }]);

        let answered = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "qualified",
            "method": "workspace/symbol",
            "params": {"query": "collections.AbstractList"}
        }));

        // The client re-filters against the name it is given. A bare `AbstractList` cannot match
        // the text the user typed, so the hit would be computed and then discarded.
        assert_eq!(
            answered.messages[0]["result"][0]["name"],
            "kotlin.collections.AbstractList"
        );
    }

    #[test]
    fn a_project_symbol_outranks_a_dependency_of_the_same_name() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        let uri = "file:///Own.kt";
        service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec![uri.to_string()],
            symbols: crate::analysis::WorkspaceSymbolIndex::from_disk_sources(&[(
                uri,
                "package demo\nclass Shared\n",
            )]),
        });
        service.set_dependency_index_for_test(crate::DependencySymbolIndex::from_internal_names([
            "vendor/Shared".to_string(),
        ]));
        service.record_located_dependencies_for_test(vec![crate::LocatedDependency {
            candidate: crate::DependencyCandidate {
                internal: "vendor/Shared".to_string(),
                package: "vendor".to_string(),
                name: "Shared".to_string(),
            },
            path: std::path::PathBuf::from("/cache/vendor/Shared.kt"),
            span: krusty::diag::Span::new(22, 28),
            text: "package vendor\n\nclass Shared\n".to_string(),
        }]);

        let answered = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "shared",
            "method": "workspace/symbol",
            "params": {"query": "Shared"}
        }));

        let results = answered.messages[0]["result"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0]["location"]["uri"], uri,
            "a name the workspace declares is the one the reader meant"
        );
        assert_eq!(results[1]["containerName"], "vendor");
    }

    #[test]
    fn a_full_project_response_does_not_queue_discarded_dependency_renders() {
        let requested = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(DependencyRecordingBackend {
            requested: requested.clone(),
        });
        service.force_initialized_for_test();
        let source = (0..crate::analysis::MAX_WORKSPACE_SYMBOL_RESPONSE_SYMBOLS)
            .map(|index| format!("class Item{index}\n"))
            .collect::<String>();
        service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec!["file:///Items.kt".to_string()],
            symbols: crate::analysis::WorkspaceSymbolIndex::from_disk_sources(&[(
                "file:///Items.kt",
                source.as_str(),
            )]),
        });
        service.set_dependency_index_for_test(crate::DependencySymbolIndex::from_internal_names([
            "vendor/ItemDependency".to_string(),
        ]));

        let answered = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "items",
            "method": "workspace/symbol",
            "params": {"query": "Item"}
        }));

        assert_eq!(
            answered.messages[0]["result"].as_array().unwrap().len(),
            crate::analysis::MAX_WORKSPACE_SYMBOL_RESPONSE_SYMBOLS
        );
        assert!(
            requested.borrow().is_empty(),
            "a dependency with no surviving response slot must not be materialized"
        );
    }

    #[test]
    fn dependency_location_work_is_bounded_while_the_engine_is_backlogged() {
        let requested = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(DependencyRecordingBackend {
            requested: requested.clone(),
        });
        service.force_initialized_for_test();
        service.set_dependency_index_for_test(crate::DependencySymbolIndex::from_internal_names(
            (0..MAX_LOCATED_DEPENDENCIES + 64).map(|index| format!("vendor/Type{index:04}")),
        ));

        // This backend deliberately never completes an attempt. Distinct exact queries would grow
        // both the service set and engine queue forever without admission at the session boundary.
        for index in 0..MAX_LOCATED_DEPENDENCIES + 64 {
            let response = service.handle(json!({
                "jsonrpc": "2.0",
                "id": index,
                "method": "workspace/symbol",
                "params": {"query": format!("Type{index:04}")}
            }));
            assert_eq!(response.messages[0]["result"], json!([]));
        }

        let queued = requested
            .borrow()
            .iter()
            .map(|(_, candidates)| candidates.len())
            .sum::<usize>();
        assert_eq!(queued, MAX_LOCATED_DEPENDENCIES);
        assert_eq!(
            service.requested_dependencies.len(),
            MAX_LOCATED_DEPENDENCIES
        );
    }

    #[test]
    fn located_dependency_eviction_preserves_unrelated_in_flight_work() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        let located = |index: usize| crate::LocatedDependency {
            candidate: crate::DependencyCandidate {
                internal: format!("vendor/Type{index}"),
                package: "vendor".to_string(),
                name: format!("Type{index}"),
            },
            path: std::path::PathBuf::from(format!("/cache/vendor/Type{index}.kt")),
            span: krusty::diag::Span::new(22, 26),
            text: format!("package vendor\n\nclass Type{index}\n"),
        };
        let initial = (0..MAX_LOCATED_DEPENDENCIES)
            .map(located)
            .collect::<Vec<_>>();
        service.record_located_dependencies(0, Vec::new(), initial);
        service
            .requested_dependencies
            .insert("vendor/StillRunning".to_string());

        service.record_located_dependencies(
            0,
            vec![format!("vendor/Type{MAX_LOCATED_DEPENDENCIES}")],
            vec![located(MAX_LOCATED_DEPENDENCIES)],
        );

        assert_eq!(service.located_dependencies.len(), MAX_LOCATED_DEPENDENCIES);
        assert!(service
            .requested_dependencies
            .contains("vendor/StillRunning"));
    }

    #[test]
    fn an_incomplete_project_symbol_index_is_reported_once() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        // Recorded the way the disk adapter records a stat-only skip; materializing a real
        // past-the-cap source would allocate the whole cap per test run.
        let oversized_skip = || {
            let mut index = crate::analysis::WorkspaceSymbolIndex::default();
            index.note_oversized_file(
                "file:///Huge.kt",
                crate::analysis::MAX_INDEXED_FILE_BYTES as u64 + 1,
            );
            index
        };

        let reported = service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec!["file:///Huge.kt".to_string()],
            symbols: oversized_skip(),
        });

        // A symbol the picker never shows is indistinguishable from one that does not exist, so
        // the shortfall has to be said out loud -- but once per cause, not per chunk.
        assert_eq!(reported.len(), 1);
        assert_eq!(reported[0]["method"], "window/logMessage");
        let message = reported[0]["params"]["message"].as_str().unwrap();
        assert!(message.contains("incomplete"), "{message}");
        // The message names the skipped file, its size, and the cap it exceeded: it is the only
        // place a user can learn which file to exclude or which limit is undersized.
        assert!(message.contains("file:///Huge.kt"), "{message}");
        assert!(message.contains("per-file cap"), "{message}");
        let repeated = service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec!["file:///Huge.kt".to_string()],
            symbols: oversized_skip(),
        });
        assert!(repeated.is_empty());
    }

    #[test]
    fn a_new_omission_cause_is_reported_even_after_the_first_report() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();

        let mut oversized_index = crate::analysis::WorkspaceSymbolIndex::default();
        oversized_index.note_oversized_file("file:///build/Gen.kt", 65 * 1024 * 1024);
        let first = service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec!["file:///build/Gen.kt".to_string()],
            symbols: oversized_index,
        });
        assert_eq!(first.len(), 1);
        let message = first[0]["params"]["message"].as_str().unwrap();
        assert!(message.contains("per-file cap"), "{message}");
        assert!(
            message.contains("file:///build/Gen.kt (65.0 MiB)"),
            "{message}"
        );

        // A zero-budget merge drops every declaration: a different omission than the skip above,
        // so it deserves its own report even though incompleteness was already said.
        let populated = crate::analysis::WorkspaceSymbolIndex::from_disk_sources(&[(
            "file:///A.kt",
            "package demo\nclass Alpha\n",
        )]);
        let mut trimmed = crate::analysis::WorkspaceSymbolIndex::default();
        trimmed.merge_within(populated, 0);
        let second = service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec!["file:///A.kt".to_string()],
            symbols: trimmed,
        });
        assert_eq!(second.len(), 1, "a new cause must be reported");
        let message = second[0]["params"]["message"].as_str().unwrap();
        assert!(message.contains("declaration(s)"), "{message}");
    }

    #[test]
    fn the_project_symbol_index_survives_analysis_batches() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        let swept_uri = "file:///Swept.kt";
        let open_uri = "file:///Open.kt";
        let open_source = "package demo\nclass OpenedType\n";
        service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec![swept_uri.to_string()],
            symbols: crate::analysis::WorkspaceSymbolIndex::from_disk_sources(&[(
                swept_uri,
                "package demo\nclass SweptType\n",
            )]),
        });
        service.open_document_for_test(open_uri, open_source, 1);

        // The live index is rebuilt from each batch; the project index must not be.
        let _ = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![(open_uri.into(), 1)],
            analyses: crate::analysis::analyze_for_lsp(&[open_source]),
            support_documents: Vec::new(),
            pending: false,
        });

        let swept = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "swept",
            "method": "workspace/symbol",
            "params": {"query": "SweptType"}
        }));
        assert_eq!(
            swept.messages[0]["result"][0]["location"]["uri"], swept_uri,
            "an analysis batch must not narrow coverage back to what is open"
        );
        let opened = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "opened",
            "method": "workspace/symbol",
            "params": {"query": "OpenedType"}
        }));
        assert_eq!(opened.messages[0]["result"][0]["location"]["uri"], open_uri);
    }

    #[test]
    fn an_open_buffer_shadows_what_its_file_says_on_disk() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        let uri = "file:///Edited.kt";
        let edited = "package demo\nclass RenamedType\n";
        service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec![uri.to_string()],
            symbols: crate::analysis::WorkspaceSymbolIndex::from_disk_sources(&[(
                uri,
                "package demo\nclass SavedType\n",
            )]),
        });
        service.open_document_for_test(uri, edited, 1);
        let _ = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![(uri.into(), 1)],
            analyses: crate::analysis::analyze_for_lsp(&[edited]),
            support_documents: Vec::new(),
            pending: false,
        });

        let renamed = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "renamed",
            "method": "workspace/symbol",
            "params": {"query": "RenamedType"}
        }));
        assert_eq!(renamed.messages[0]["result"][0]["location"]["uri"], uri);
        let saved = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "saved",
            "method": "workspace/symbol",
            "params": {"query": "SavedType"}
        }));
        assert_eq!(
            saved.messages[0]["result"],
            json!([]),
            "the buffer's current text wins over the copy the sweep read from disk"
        );
    }

    /// Retain `uri` in the project layer with `disk`, then open it with `buffer` and analyze.
    fn service_with_edited_buffer(
        uri: &str,
        disk: &str,
        buffer: &str,
    ) -> LspService<RecordingBackend> {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec![uri.to_string()],
            symbols: crate::analysis::WorkspaceSymbolIndex::from_disk_sources(&[(uri, disk)]),
        });
        service.open_document_for_test(uri, buffer, 1);
        let _ = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![(uri.into(), 1)],
            analyses: crate::analysis::analyze_for_lsp(&[buffer]),
            support_documents: Vec::new(),
            pending: false,
        });
        service
    }

    fn workspace_symbol_result(service: &mut LspService<RecordingBackend>, query: &str) -> Value {
        service
            .handle(json!({
                "jsonrpc": "2.0",
                "id": "workspace-symbol",
                "method": "workspace/symbol",
                "params": {"query": query}
            }))
            .messages[0]["result"]
            .clone()
    }

    #[test]
    fn a_buffer_edited_down_to_no_declarations_still_shadows_its_file_on_disk() {
        // The live index names only the files some entry references, so a buffer that declares
        // nothing names nothing. Shadowing has to come from the open set too, or the copy the sweep
        // read from disk answers for a file whose buffer no longer declares it.
        let mut service = service_with_edited_buffer(
            "file:///Gone.kt",
            "package demo\nclass GoneType\n",
            "package demo\n",
        );

        assert_eq!(
            workspace_symbol_result(&mut service, "GoneType"),
            json!([]),
            "an open buffer must shadow its file on disk even when it declares nothing"
        );
    }

    #[test]
    fn closing_a_document_stops_serving_its_buffer_and_restores_the_file_on_disk() {
        let uri = "file:///Edited.kt";
        let mut service = service_with_edited_buffer(
            uri,
            "package demo\nclass SavedType\n",
            "package demo\nclass RenamedType\n",
        );

        let _ = service.handle_deferred(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": {"textDocument": {"uri": uri}}
        }));

        // Closing discards the buffer, so what it declared goes with it -- immediately, not once a
        // replacement batch happens to land.
        assert_eq!(
            workspace_symbol_result(&mut service, "RenamedType"),
            json!([]),
            "an abandoned buffer must not keep answering after its document closes"
        );
        assert_eq!(
            workspace_symbol_result(&mut service, "SavedType")[0]["location"]["uri"],
            uri,
            "and the file on disk must become visible again"
        );
    }

    #[test]
    fn workspace_symbols_answer_from_the_index_after_the_last_document_closes() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        let uri = "file:///Closed.kt";
        let source = "class ClosedMarker\n";
        service.apply_symbol_index_batch(SymbolIndexBatch {
            generation: 0,
            attempted: vec![uri.to_string()],
            symbols: crate::analysis::WorkspaceSymbolIndex::from_disk_sources(&[(uri, source)]),
        });
        service.open_document_for_test(uri, source, 1);
        let _ = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![(uri.into(), 1)],
            analyses: crate::analysis::analyze_for_lsp(&[source]),
            support_documents: Vec::new(),
            pending: false,
        });

        let _ = service.handle_deferred(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": {"textDocument": {"uri": uri}}
        }));
        let answered = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "workspace-after-close",
            "method": "workspace/symbol",
            "params": {"query": "ClosedMarker"}
        }));

        // Closing the last document used to park the query until a replacement snapshot arrived.
        // The project index covers the file whether or not anything has it open, so there is
        // nothing left to wait for.
        assert_eq!(answered.messages.len(), 1);
        assert_eq!(answered.messages[0]["id"], "workspace-after-close");
        assert_eq!(
            answered.messages[0]["result"][0]["location"]["uri"], uri,
            "the closed file must still be findable"
        );
    }

    #[test]
    fn definition_waits_for_the_current_analysis_batch() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        let uri = "file:///document.kt";
        let source = "val target = 1\nval use = target\n";
        service.open_document_for_test(uri, source, 1);
        service.mark_analysis_dirty_for_test();
        let _job = service
            .dispatch_pending_analysis()
            .expect("analysis starts for the open document");

        let pending = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "definition",
            "method": "textDocument/definition",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 10}
            }
        }));
        assert!(pending.messages.is_empty());

        let messages = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![(uri.into(), 1)],
            analyses: crate::analysis::analyze_for_lsp(&[source]),
            support_documents: Vec::new(),
            pending: false,
        });
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["id"], "definition");
        assert!(messages[1]["result"]
            .as_array()
            .is_some_and(|locations| !locations.is_empty()));
    }

    #[test]
    fn only_dev_code_actions_wait_for_the_current_analysis_batch() {
        let uri = "file:///document.kt";
        let source = "fun box(): String = \"OK\"\n";
        let analysis_in_flight = |dev| {
            let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let mut service = LspService::with_backend(RecordingBackend {
                ready: true,
                submitted,
            })
            .with_dev(dev);
            service.force_initialized_for_test();
            service.open_document_for_test(uri, source, 1);
            service.mark_analysis_dirty_for_test();
            let _job = service
                .dispatch_pending_analysis()
                .expect("analysis starts for the open document");
            service
        };

        let mut dev_service = analysis_in_flight(true);

        let pending = dev_service.handle(dump_code_action_request(7, uri, 0, 0));
        assert!(
            pending.messages.is_empty(),
            "a dump must not inspect retained input while its replacement analysis is in flight"
        );
        assert_eq!(dev_service.pending_analysis_requests.len(), 1);

        let messages = dev_service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![(uri.into(), 1)],
            analyses: vec![DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        });
        assert_eq!(messages.len(), 2, "one diagnostic publish plus the action");
        assert_eq!(messages[1]["id"], 7);
        assert_eq!(
            messages[1]["result"],
            json!([]),
            "the recording backend has no dump, but it is consulted only after analysis completes"
        );

        let mut ordinary_service = analysis_in_flight(false);
        let immediate = ordinary_service.handle(dump_code_action_request(8, uri, 0, 0));
        assert_eq!(immediate.messages.len(), 1);
        assert_eq!(immediate.messages[0]["id"], 8);
        assert_eq!(immediate.messages[0]["result"], json!([]));
        assert!(
            ordinary_service.pending_analysis_requests.is_empty(),
            "non-dev mode has no action whose answer could depend on analysis"
        );
    }

    #[test]
    fn document_change_cancels_pending_analysis_requests() {
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
        assert!(service
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "textDocument/definition",
                "params": {
                    "textDocument": {"uri": "file:///a.kt"},
                    "position": {"line": 0, "character": 0}
                }
            }))
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

        assert_eq!(changed.messages.len(), 2);
        assert_eq!(changed.messages[0]["id"], 7);
        assert_eq!(changed.messages[0]["error"]["code"], -32802);
        assert_eq!(
            changed.messages[0]["error"]["data"]["retriggerRequest"],
            true
        );
        assert_eq!(changed.messages[1]["id"], 8);
        assert_eq!(changed.messages[1]["error"]["code"], -32801);
        assert!(service.pending_analysis_requests.is_empty());
        assert_eq!(service.pending_analysis_request_bytes, 0);
    }

    #[test]
    fn pending_analysis_requests_share_the_byte_budget() {
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

        let id_bytes = MAX_PENDING_ANALYSIS_REQUEST_BYTES / 3;
        let diagnostic = service.pull_diagnostics(
            Some(Value::String("a".repeat(id_bytes))),
            json!({"textDocument": {"uri": "file:///a.kt"}}),
        );
        assert!(diagnostic.messages.is_empty());
        let definition = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "b".repeat(id_bytes),
            "method": "textDocument/definition",
            "params": {
                "textDocument": {"uri": "file:///b.kt"},
                "position": {"line": 0, "character": 0}
            }
        }));
        assert!(definition.messages.is_empty());

        let rejected = service.pull_diagnostics(
            Some(Value::String("c".repeat(id_bytes))),
            json!({"textDocument": {"uri": "file:///c.kt"}}),
        );
        assert_eq!(rejected.messages.len(), 1);
        assert_eq!(rejected.messages[0]["error"]["code"], -32802);
        assert_eq!(service.pending_analysis_requests.len(), 2);
        assert!(service.pending_analysis_request_bytes <= MAX_PENDING_ANALYSIS_REQUEST_BYTES);
    }

    #[test]
    fn pending_exact_requests_share_the_existing_response_budget() {
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut service = LspService::with_backend(RecordingBackend {
            ready: true,
            submitted,
        });
        service.force_initialized_for_test();
        let uri = "file:///document.kt";
        service.open_document_for_test(uri, "val value = 1\n", 1);
        service.mark_analysis_dirty_for_test();
        let _job = service
            .dispatch_pending_analysis()
            .expect("analysis starts for the open document");

        let tokens = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "tokens",
            "method": "textDocument/semanticTokens/full",
            "params": {"textDocument": {"uri": uri}}
        }));
        assert!(tokens.messages.is_empty());

        let definition = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "definition",
            "method": "textDocument/definition",
            "params": {
                "textDocument": {"uri": uri},
                "position": {"line": 0, "character": 4}
            }
        }));
        assert_eq!(definition.messages[0]["error"]["code"], -32802);
        assert_eq!(service.pending_analysis_requests.len(), 1);
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
        assert!(service.pending_analysis_requests.is_empty());
        assert_eq!(service.pending_analysis_request_bytes, 0);

        let exited = service.handle(json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }));
        assert!(exited.exit);
        assert_eq!(exited.exit_code, 0);
    }

    #[test]
    fn mixed_stale_batch_applies_current_local_results_without_replacing_the_snapshot() {
        let mut service = LspService::new(|s: &[&str]| {
            s.iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "v1", 1);
        service.open_document_for_test("file:///b.kt", "v1", 1);
        service.source_set = vec![
            ("file:///a.kt".into(), "v1".into()),
            ("file:///b.kt".into(), "v1".into()),
        ];
        service
            .documents
            .get_mut("file:///a.kt")
            .unwrap()
            .definitions = DefinitionIndex::wire_saturation_fixture(1);
        service.mark_analysis_dirty_for_test();
        service
            .dispatch_pending_analysis()
            .expect("job for the open documents");
        assert!(service
            .pull_diagnostics(
                Some(json!(7)),
                json!({"textDocument": {"uri": "file:///a.kt"}}),
            )
            .messages
            .is_empty());

        service.open_document_for_test("file:///b.kt", "v2", 2);

        let mut a_analysis = analysis_with_diagnostic("current");
        a_analysis.definitions = DefinitionIndex::wire_saturation_fixture(2);
        let batch = AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1), ("file:///b.kt".into(), 1)],
            analyses: vec![a_analysis, DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        };
        let messages = service.apply_analysis_batch(batch);
        let published = messages
            .iter()
            .filter(|message| message["method"] == "textDocument/publishDiagnostics")
            .map(|message| message["params"]["uri"].clone())
            .collect::<Vec<_>>();
        assert_eq!(published, vec![json!("file:///a.kt")]);
        assert_eq!(
            service.source_set,
            vec![
                ("file:///a.kt".into(), "v1".into()),
                ("file:///b.kt".into(), "v1".into()),
            ]
        );
        assert_eq!(
            service.documents["file:///a.kt"].definitions.entry_count(),
            1
        );
        assert_eq!(
            service.document_diagnostic_count_for_test("file:///a.kt"),
            1
        );
        assert_eq!(service.pending_analysis_requests.len(), 1);
        assert!(!messages.iter().any(|message| message["id"] == 7));
        assert!(service.analysis_dirty_for_test());
        assert!(!service.analysis_in_flight_for_test());
    }

    #[test]
    fn closing_one_document_keeps_the_in_flight_analysis_of_the_others() {
        let mut service = LspService::new(|s: &[&str]| {
            s.iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "v1", 1);
        service.open_document_for_test("file:///b.kt", "v1", 1);
        service.mark_analysis_dirty_for_test();
        service
            .dispatch_pending_analysis()
            .expect("job for the open documents");

        service.handle(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": {"textDocument": {"uri": "file:///b.kt"}},
        }));

        let messages = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1), ("file:///b.kt".into(), 1)],
            analyses: vec![DocumentAnalysis::empty(), DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        });
        let published = messages
            .iter()
            .filter(|message| message["method"] == "textDocument/publishDiagnostics")
            .map(|message| message["params"]["uri"].clone())
            .collect::<Vec<_>>();
        assert_eq!(published, vec![json!("file:///a.kt")]);
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
        assert_eq!(service.pending_analysis_requests.len(), 1);

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
        assert!(service.pending_analysis_requests.is_empty());
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
    fn matching_diagnostic_result_id_returns_unchanged() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "fun a() {}", 1);
        service.take_analysis_job();
        service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![analysis_with_diagnostic("boom")],
            support_documents: Vec::new(),
            pending: false,
        });

        let first = service.pull_diagnostics(
            Some(json!(1)),
            json!({"textDocument": {"uri": "file:///a.kt"}}),
        );
        let report = &first.messages[0]["result"];
        assert_eq!(report["kind"], "full");
        let result_id = report["resultId"]
            .as_str()
            .expect("full diagnostic result id")
            .to_string();

        service.mark_analysis_dirty_for_test();
        service
            .dispatch_pending_analysis()
            .expect("diagnostic refresh analysis");
        let pending = service.pull_diagnostics(
            Some(json!(2)),
            json!({"textDocument": {"uri": "file:///a.kt"}, "previousResultId": result_id.clone()}),
        );
        assert!(pending.messages.is_empty());
        let completed = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![analysis_with_diagnostic("boom")],
            support_documents: Vec::new(),
            pending: false,
        });
        let response = completed
            .iter()
            .find(|message| message["id"] == 2)
            .expect("pending diagnostic response");
        assert_eq!(response["result"]["kind"], "unchanged");
        assert_eq!(
            response["result"]["resultId"].as_str(),
            Some(result_id.as_str())
        );
    }

    #[test]
    fn stale_diagnostic_result_id_returns_full() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "fun a() {}", 1);
        service.take_analysis_job();
        service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![analysis_with_diagnostic("before")],
            support_documents: Vec::new(),
            pending: false,
        });
        let initial = service.pull_diagnostics(
            Some(json!(1)),
            json!({"textDocument": {"uri": "file:///a.kt"}}),
        );
        let previous_result_id = initial.messages[0]["result"]["resultId"]
            .as_str()
            .expect("initial diagnostic result id")
            .to_string();

        service.mark_analysis_dirty_for_test();
        service
            .dispatch_pending_analysis()
            .expect("replacement diagnostic analysis");
        service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![analysis_with_diagnostic("after")],
            support_documents: Vec::new(),
            pending: false,
        });

        let report = service.pull_diagnostics(
            Some(json!(2)),
            json!({
                "textDocument": {"uri": "file:///a.kt"},
                "previousResultId": previous_result_id.clone()
            }),
        );
        assert_eq!(report.messages[0]["result"]["kind"], "full");
        assert_ne!(
            report.messages[0]["result"]["resultId"].as_str(),
            Some(previous_result_id.as_str())
        );
    }

    #[test]
    fn pull_client_without_refresh_is_not_pushed_diagnostics() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {
                    "textDocument": { "diagnostic": { "dynamicRegistration": false } },
                }
            },
        }));
        service.open_document_for_test("file:///a.kt", "v1", 1);
        service.take_analysis_job();

        let messages = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        });
        assert!(
            !messages
                .iter()
                .any(|message| message["method"] == "textDocument/publishDiagnostics"),
            "a pull client must not also be pushed diagnostics: {messages:?}"
        );
        assert!(
            !messages
                .iter()
                .any(|message| message["method"] == "workspace/diagnostic/refresh"),
            "without refresh support there is nothing to request: {messages:?}"
        );
    }

    #[test]
    fn pull_capable_client_is_not_also_pushed_diagnostics() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis {
                    diagnostics: vec![Diagnostic {
                        span: krusty::diag::Span::new(0, 1),
                        editor_span: None,
                        identity: None,
                        severity: Severity::Error,
                        kind: DiagnosticKind::Compiler,
                        msg: "boom".to_string(),
                        file: 0,
                    }],
                    ..DocumentAnalysis::empty()
                })
                .collect::<Vec<_>>()
        });
        service.handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {
                    "textDocument": { "diagnostic": { "dynamicRegistration": false } },
                    "workspace": { "diagnostics": { "refreshSupport": true } },
                }
            },
        }));
        service.did_open(
            None,
            json!({
                "textDocument": {
                    "uri": "file:///a.kt", "languageId": "kotlin", "version": 1, "text": "fun a() {}"
                }
            }),
            true,
        );
        service.take_analysis_job();

        let messages = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis {
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 1),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: "boom".to_string(),
                    file: 0,
                }],
                ..DocumentAnalysis::empty()
            }],
            support_documents: Vec::new(),
            pending: false,
        });
        assert!(
            !messages
                .iter()
                .any(|message| message["method"] == "textDocument/publishDiagnostics"),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|message| message["method"] == "workspace/diagnostic/refresh"),
            "{messages:?}"
        );

        let pulled = service.pull_diagnostics(
            Some(json!(2)),
            json!({ "textDocument": { "uri": "file:///a.kt" } }),
        );
        assert_eq!(
            pulled.messages[0]["result"]["items"]
                .as_array()
                .map(Vec::len),
            Some(1),
            "{:?}",
            pulled.messages
        );
    }

    #[test]
    fn workspace_diagnostic_clears_open_documents_for_pull_clients() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {
                    "textDocument": { "diagnostic": { "dynamicRegistration": false } },
                    "workspace": { "diagnostics": { "refreshSupport": true } },
                }
            },
        }));
        service.open_document_for_test("file:///a.kt", "x", 1);
        service.take_analysis_job();

        let messages = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis {
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 1),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: "boom".to_string(),
                    file: 0,
                }],
                ..DocumentAnalysis::empty()
            }],
            support_documents: Vec::new(),
            pending: false,
        });
        assert!(
            !messages
                .iter()
                .any(|message| message["method"] == "textDocument/publishDiagnostics"),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|message| message["method"] == "workspace/diagnostic/refresh"),
            "{messages:?}"
        );

        let workspace = service.workspace_diagnostic(Some(json!(2)), json!({}));
        let items = workspace.messages[0]["result"]["items"]
            .as_array()
            .expect("workspace diagnostic items");
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0]["uri"], "file:///a.kt");
        assert_eq!(items[0]["kind"], "full");
        assert_eq!(items[0]["items"], json!([]));

        let pulled = service.pull_diagnostics(
            Some(json!(3)),
            json!({ "textDocument": { "uri": "file:///a.kt" } }),
        );
        assert_eq!(
            pulled.messages[0]["result"]["items"]
                .as_array()
                .map(Vec::len),
            Some(1),
            "{:?}",
            pulled.messages
        );
    }

    #[test]
    fn diagnostic_refresh_requests_are_coalesced() {
        let mut service = LspService::new(|_: &[&str]| Vec::new());
        service.client_pulls_diagnostics = true;
        service.client_refreshes_diagnostics = true;

        let refresh = service.diagnostic_refresh().unwrap();
        assert_eq!(refresh["method"], "workspace/diagnostic/refresh");
        assert!(
            refresh.get("params").is_none(),
            "the parameterless LSP request must omit params: {refresh:?}"
        );
        assert!(service.diagnostic_refresh().is_none());

        let response = service.handle(json!({
            "jsonrpc": "2.0",
            "id": DIAGNOSTIC_REFRESH_REQUEST_ID,
            "result": null,
        }));
        assert_eq!(response.messages.len(), 1);
        assert_eq!(
            response.messages[0]["method"],
            "workspace/diagnostic/refresh"
        );
    }

    #[test]
    fn push_only_client_still_receives_published_diagnostics() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": { "textDocument": {} } },
        }));
        service.did_open(
            None,
            json!({
                "textDocument": {
                    "uri": "file:///a.kt", "languageId": "kotlin", "version": 1, "text": "fun a() {}"
                }
            }),
            true,
        );
        service.take_analysis_job();

        let messages = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis::empty()],
            support_documents: Vec::new(),
            pending: false,
        });
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
                identity: None,
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
                identity: None,
                severity: Severity::Error,
                kind: DiagnosticKind::Compiler,
                msg: "same message".to_string(),
                file: 0,
            },
            Diagnostic {
                span: krusty::diag::Span::new(2, 3),
                editor_span: None,
                identity: None,
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
            identity: None,
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
                identity: None,
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
                identity: None,
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
            file_uri_to_path("file:///workspace/pro%20ject"),
            Some(PathBuf::from("/workspace/pro ject"))
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
    #[test]
    fn a_pull_for_an_unopened_file_answers_from_the_workspace_index() {
        let mut service = LspService::new(|s: &[&str]| {
            s.iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec!["file:///w/Swept.kt".to_string()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: "file:///w/Swept.kt".to_string(),
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(4, 9),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: "swept boom".to_string(),
                    file: 0,
                }],
                text_hash: 3,
                text: "val broken = 1\n".to_string(),
            }],
        });

        let report = service.pull_diagnostics(
            Some(json!(1)),
            json!({"textDocument": {"uri": "file:///w/Swept.kt"}}),
        );
        let items = &report.messages[0]["result"]["items"];
        assert_eq!(items.as_array().map(Vec::len), Some(1));
        assert_eq!(items[0]["message"], "Swept boom");
        assert_eq!(items[0]["range"]["start"]["line"], 0);
        assert_eq!(
            items[0]["range"]["start"]["character"], 4,
            "byte spans must be resolved to UTF-16 columns while the text is still in hand"
        );
    }

    #[test]
    fn an_attempted_file_that_produced_no_result_loses_its_retained_diagnostics() {
        let mut service = LspService::new(|s: &[&str]| {
            s.iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        let uri = "file:///w/Gone.kt".to_string();
        service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec![uri.clone()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: uri.clone(),
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 1),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: "will be deleted".to_string(),
                    file: 0,
                }],
                text_hash: 1,
                text: "x\n".to_string(),
            }],
        });

        // The file is deleted, so the next sweep attempts it and produces nothing for it.
        service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec![uri.clone()],
            conclusive: true,
            files: Vec::new(),
        });

        let report =
            service.pull_diagnostics(Some(json!(2)), json!({"textDocument": {"uri": uri}}));
        assert_eq!(
            report.messages[0]["result"]["items"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "a deleted file must not keep reporting diagnostics forever"
        );
    }

    #[test]
    fn an_index_batch_from_a_replaced_model_is_rejected() {
        let mut service = LspService::new(|s: &[&str]| {
            s.iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.apply_index_batch(IndexBatch {
            generation: 4,
            attempted: vec!["file:///new/A.kt".to_string()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: "file:///new/A.kt".to_string(),
                diagnostics: Vec::new(),
                text_hash: 1,
                text: String::new(),
            }],
        });
        service.apply_index_batch(IndexBatch {
            generation: 1,
            attempted: vec!["file:///new/A.kt".to_string()],
            conclusive: true,
            files: Vec::new(),
        });

        assert!(
            service
                .workspace_diagnostics
                .diagnostics("file:///new/A.kt")
                .is_some(),
            "a late batch from a replaced model must not delete data the current model produced"
        );
    }

    #[test]
    fn a_model_reset_clears_retained_diagnostics_before_the_replacement_sweep() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        let uri = "file:///old/Model.kt";
        service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec![uri.to_string()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: uri.to_string(),
                diagnostics: Vec::new(),
                text_hash: 1,
                text: String::new(),
            }],
        });

        let _refresh = service.reset_workspace_index(1);

        assert!(
            service.workspace_diagnostics.diagnostics(uri).is_none(),
            "old-model results must disappear when the model changes, not when its first new batch arrives"
        );
        service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec![uri.to_string()],
            conclusive: true,
            files: Vec::new(),
        });
        assert!(
            service.workspace_diagnostics.diagnostics(uri).is_none(),
            "a late old-generation batch must not repopulate the cleared store"
        );
    }

    #[test]
    fn workspace_report_honors_prior_ids_and_clears_disappeared_files() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        let indexed_uri = "file:///w/Indexed.kt";
        service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec![indexed_uri.to_string()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: indexed_uri.to_string(),
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 1),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: "indexed".to_string(),
                    file: 0,
                }],
                text_hash: 1,
                text: "x".to_string(),
            }],
        });
        let first = service.workspace_diagnostic(Some(json!(1)), json!({}));
        let prior = first.messages[0]["result"]["items"][0]["resultId"]
            .as_str()
            .expect("workspace result id")
            .to_string();

        let second = service.workspace_diagnostic(
            Some(json!(2)),
            json!({
                "previousResultIds": [
                    {"uri": indexed_uri, "value": prior},
                    {"uri": "file:///w/Deleted.kt", "value": "stale"},
                ]
            }),
        );
        let items = second.messages[0]["result"]["items"]
            .as_array()
            .expect("workspace diagnostic items");
        let indexed = items
            .iter()
            .find(|item| item["uri"] == indexed_uri)
            .expect("indexed report");
        assert_eq!(indexed["kind"], "unchanged");
        assert_eq!(indexed["version"], Value::Null);
        let deleted = items
            .iter()
            .find(|item| item["uri"] == "file:///w/Deleted.kt")
            .expect("deleted-file tombstone");
        assert_eq!(deleted["kind"], "full");
        assert_eq!(deleted["items"], json!([]));
        assert_eq!(deleted["version"], Value::Null);
        assert!(
            serialized_value_fits(&second.messages[0], MAX_MESSAGE_BYTES),
            "workspace diagnostic response must respect the framed protocol limit"
        );
    }

    #[test]
    fn workspace_report_uses_the_open_buffer_over_retained_disk_results() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        let uri = "file:///w/Open.kt";
        service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec![uri.to_string()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: uri.to_string(),
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 1),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: "stale disk diagnostic".to_string(),
                    file: 0,
                }],
                text_hash: 1,
                text: "x".to_string(),
            }],
        });
        service.open_document_for_test(uri, "val current = 1", 1);

        let report = service.workspace_diagnostic(Some(json!(1)), json!({}));
        let item = &report.messages[0]["result"]["items"][0];
        assert_eq!(item["uri"], uri);
        assert_eq!(
            item["items"],
            json!([]),
            "the current open buffer must win over an older sweep snapshot"
        );
    }
    #[test]
    fn each_indexed_chunk_publishes_its_diagnostics_immediately() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        let messages = service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec!["file:///w/Swept.kt".to_string()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: "file:///w/Swept.kt".to_string(),
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 3),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: "swept boom".to_string(),
                    file: 0,
                }],
                text_hash: 1,
                text: "val x = 1\n".to_string(),
            }],
        });

        let published = messages
            .iter()
            .find(|message| message["method"] == "textDocument/publishDiagnostics")
            .expect("a sweep that runs for hours must publish as it goes, not only at the end");
        assert_eq!(published["params"]["uri"], "file:///w/Swept.kt");
        assert_eq!(
            published["params"]["diagnostics"].as_array().map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn indexed_chunk_does_not_publish_for_pull_refresh_client() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {
                    "textDocument": { "diagnostic": { "dynamicRegistration": false } },
                    "workspace": { "diagnostics": { "refreshSupport": true } },
                }
            },
        }));

        let messages = service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec!["file:///w/Swept.kt".to_string()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: "file:///w/Swept.kt".to_string(),
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 3),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: "swept boom".to_string(),
                    file: 0,
                }],
                text_hash: 1,
                text: "val x = 1\n".to_string(),
            }],
        });

        assert!(
            !messages
                .iter()
                .any(|message| message["method"] == "textDocument/publishDiagnostics"),
            "a pull+refresh client must not be pushed the same diagnostics it will pull: {messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|message| message["method"] == "workspace/diagnostic/refresh"),
            "{messages:?}"
        );
    }

    #[test]
    fn indexed_chunk_does_not_publish_for_pull_client_without_refresh() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {
                    "textDocument": { "diagnostic": { "dynamicRegistration": false } },
                }
            },
        }));

        let messages = service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec!["file:///w/Swept.kt".to_string()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: "file:///w/Swept.kt".to_string(),
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 3),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: "swept boom".to_string(),
                    file: 0,
                }],
                text_hash: 1,
                text: "val x = 1\n".to_string(),
            }],
        });

        assert!(
            !messages
                .iter()
                .any(|message| message["method"] == "textDocument/publishDiagnostics"),
            "a pull client must not also be pushed diagnostics: {messages:?}"
        );
        assert!(
            !messages
                .iter()
                .any(|message| message["method"] == "workspace/diagnostic/refresh"),
            "without refresh support the server has no way to prompt a re-pull: {messages:?}"
        );

        // The diagnostic is still reachable through the pull channel.
        let report = service.pull_diagnostics(
            Some(json!(2)),
            json!({"textDocument": {"uri": "file:///w/Swept.kt"}}),
        );
        let items = &report.messages[0]["result"]["items"];
        assert_eq!(items.as_array().map(Vec::len), Some(1));
        assert_eq!(items[0]["message"], "Swept boom");
    }

    #[test]
    fn an_open_document_is_not_republished_by_the_sweep() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///w/Open.kt", "fun a() {}", 1);

        let messages = service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec!["file:///w/Open.kt".to_string()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: "file:///w/Open.kt".to_string(),
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 1),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: "stale from disk".to_string(),
                    file: 0,
                }],
                text_hash: 1,
                text: "fun a() {}\n".to_string(),
            }],
        });

        assert!(
            !messages
                .iter()
                .any(|message| message["method"] == "textDocument/publishDiagnostics"),
            "the buffer the user is editing must not be overwritten by what the sweep read"
        );
    }

    #[test]
    fn a_deleted_indexed_file_pushes_an_empty_diagnostic_set() {
        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        let uri = "file:///w/Removed.kt".to_string();
        service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec![uri.clone()],
            conclusive: true,
            files: vec![IndexedFile {
                uri: uri.clone(),
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 1),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: "removed diagnostic".to_string(),
                    file: 0,
                }],
                text_hash: 1,
                text: "x\n".to_string(),
            }],
        });

        let messages = service.apply_index_batch(IndexBatch {
            generation: 0,
            attempted: vec![uri.clone()],
            conclusive: true,
            files: Vec::new(),
        });
        let published = messages
            .iter()
            .find(|message| message["method"] == "textDocument/publishDiagnostics")
            .expect("removing retained diagnostics must clear the client's pushed copy");
        assert_eq!(published["params"]["uri"], uri);
        assert_eq!(
            published["params"]["diagnostics"].as_array().map(Vec::len),
            Some(0)
        );
    }
}
