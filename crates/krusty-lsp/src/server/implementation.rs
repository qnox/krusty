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
    HoverIndex, SemanticTokenIndex, SemanticTokenRange, SignatureHelpIndex,
    SEMANTIC_TOKEN_MODIFIERS, SEMANTIC_TOKEN_TYPES,
};
use crate::uri::file_uri_to_path;
use crate::worker::{source_set_fits, MAX_SOURCE_SET_BYTES};
use krusty::diag::{Diagnostic, Severity};

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_HEADER_BYTES: usize = 8 * 1024;
const INPUT_QUEUE_CAPACITY: usize = 4;
const MAX_OPEN_DOCUMENTS: usize = 256;
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
const MAX_RENAME_IDENTIFIER_BYTES: usize = 1024;
const MAX_RENAME_SPELLINGS: usize = 8;
pub(super) const MAX_RENAME_WIRE_BYTES: usize = 8 * 1024 * 1024;
const RENAME_DOCUMENT_WIRE_FIXED_BYTES: usize = 128;
const RENAME_EDIT_WIRE_FIXED_BYTES: usize = 192;
const DIAGNOSTIC_WARNING_BIT: u32 = 1 << 31;
const DIAGNOSTIC_MESSAGE_MASK: u32 = !DIAGNOSTIC_WARNING_BIT;
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(150);
const MAX_BATCH_DURATION: Duration = Duration::from_millis(500);
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

/// Analysis and project-model operations behind an LSP session.
pub trait Analysis {
    fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis>;

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

    fn project_refresh_due_in(&self) -> Option<Duration> {
        None
    }

    fn refresh_project(&mut self) -> ProjectFeedback {
        ProjectFeedback::default()
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
            pending.push([
                diagnostic.span.lo,
                diagnostic.span.hi.max(diagnostic.span.lo),
                severity | message_id,
            ]);
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
                json!({
                    "range": {
                        "start": {"line": entry[0], "character": entry[1]},
                        "end": {"line": entry[2], "character": entry[3]},
                    },
                    "severity": if entry[4] & DIAGNOSTIC_WARNING_BIT == 0 { 1 } else { 2 },
                    "source": "Kotlin",
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
    document_symbols: DocumentSymbolIndex,
    folding_ranges: FoldingRangeIndex,
    analysis_blocked: bool,
}

/// Stateful LSP dispatcher with an injected analysis function for deterministic unit testing.
pub struct LspService<A> {
    documents: HashMap<String, OpenDocument>,
    analyze: A,
    analysis_dirty: bool,
    initialized: bool,
    shutdown_requested: bool,
    pending_init_feedback: Option<ProjectFeedback>,
}

impl<A> LspService<A>
where
    A: Analysis,
{
    pub fn new(analyze: A) -> Self {
        Self {
            documents: HashMap::new(),
            analyze,
            analysis_dirty: false,
            initialized: false,
            shutdown_requested: false,
            pending_init_feedback: None,
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
        source_set_fits(
            self.documents
                .iter()
                .filter_map(|(open_uri, document)| (open_uri != uri).then_some(document.text.len()))
                .chain(std::iter::once(text_len)),
        )
    }

    fn refresh_documents(&mut self) -> Vec<Value> {
        let mut diagnostic_budget = DiagnosticBudget::default();
        let uris = self
            .analyzed_uris()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let analyses = {
            let documents = &self.documents;
            let sources: Vec<_> = uris
                .iter()
                .map(|uri| documents[uri].text.as_str())
                .collect();
            self.analyze.analyze(&sources)
        };
        if analyses.len() != uris.len() {
            for uri in &uris {
                let open = self.documents.get_mut(uri).unwrap();
                open.hover = HoverIndex::default();
                open.completion = CompletionIndex::default();
                open.signature_help = SignatureHelpIndex::default();
                open.semantic_tokens = SemanticTokenIndex::default();
                open.definitions = DefinitionIndex::default();
                open.type_definitions = DefinitionIndex::default();
                open.document_symbols = DocumentSymbolIndex::default();
                open.folding_ranges = FoldingRangeIndex::default();
                open.diagnostics = DiagnosticIndex::from_diagnostics(
                    vec![Diagnostic {
                        span: krusty::diag::Span::new(0, 0),
                        severity: Severity::Error,
                        msg: "analysis worker returned an incomplete source set".to_string(),
                        file: 0,
                    }],
                    &open.text,
                    &mut diagnostic_budget,
                );
            }
            return uris
                .into_iter()
                .map(|uri| {
                    let open = &self.documents[&uri];
                    publish_diagnostics(&uri, Some(open.version), &open.diagnostics)
                })
                .collect();
        }
        uris.into_iter()
            .zip(analyses)
            .map(|(uri, analysis)| {
                let open = self.documents.get_mut(&uri).unwrap();
                open.hover = analysis.hover;
                open.completion = analysis.completion;
                open.signature_help = analysis.signature_help;
                open.semantic_tokens = analysis.semantic_tokens;
                open.definitions = analysis.definitions;
                open.type_definitions = analysis.type_definitions;
                open.document_symbols = analysis.document_symbols;
                open.folding_ranges = analysis.folding_ranges;
                open.diagnostics = DiagnosticIndex::from_diagnostics(
                    analysis.diagnostics,
                    &open.text,
                    &mut diagnostic_budget,
                );
                publish_diagnostics(&uri, Some(open.version), &open.diagnostics)
            })
            .collect()
    }

    fn flush_analysis(&mut self) -> Vec<Value> {
        if !std::mem::take(&mut self.analysis_dirty) {
            return Vec::new();
        }
        self.refresh_documents()
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
                self.pending_init_feedback =
                    Some(self.analyze.set_workspace_root(workspace_root(&params)));
                Dispatch::messages(vec![rpc_result(
                    id,
                    json!({
                        "capabilities": {
                            "hoverProvider": true,
                            "definitionProvider": true,
                            "typeDefinitionProvider": true,
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
                let mut messages = Vec::new();
                let globs = self.analyze.watched_globs();
                if !globs.is_empty() {
                    messages.push(register_watched_files(&globs));
                }
                if let Some(feedback) = self.pending_init_feedback.take() {
                    messages.extend(feedback.into_messages());
                }
                Dispatch::messages(messages)
            }
            "workspace/didChangeWatchedFiles" => self.did_change_watched_files(),
            "textDocument/didOpen" => self.did_open(id, params, defer_analysis),
            "textDocument/didChange" => self.did_change(id, params, defer_analysis),
            "textDocument/didClose" => self.did_close(id, params, defer_analysis),
            "textDocument/hover" => self.hover(id, params),
            "textDocument/definition" => self.definition(id, params),
            "textDocument/typeDefinition" => self.type_definition(id, params),
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
                Dispatch::messages(vec![rpc_result(id, Value::Null)])
            }
            _ => match id {
                Some(id) => Dispatch::messages(vec![rpc_error(id, -32601, "method not found")]),
                None => Dispatch::none(),
            },
        }
    }

    fn did_change_watched_files(&mut self) -> Dispatch {
        self.analyze.note_project_change();
        Dispatch::none()
    }

    pub fn project_refresh_due_in(&self) -> Option<Duration> {
        self.analyze.project_refresh_due_in()
    }

    pub fn run_due_project_refresh(&mut self) -> Vec<Value> {
        let feedback = self.analyze.refresh_project();
        let reanalyze = feedback.reanalyze;
        let mut messages = feedback.into_messages();
        if reanalyze {
            self.analysis_dirty = true;
            messages.extend(self.flush_analysis());
        }
        messages
    }

    fn did_open(&mut self, id: Option<Value>, params: Value, defer_analysis: bool) -> Dispatch {
        let Ok(params) = serde_json::from_value::<DidOpenParams>(params) else {
            return invalid_params(id);
        };
        let uri = params.text_document.uri;
        let version = params.text_document.version;
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
            let mut messages = vec![publish_diagnostics(&uri, Some(version), diagnostics)];
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
                document_symbols: DocumentSymbolIndex::default(),
                folding_ranges: FoldingRangeIndex::default(),
                analysis_blocked: false,
            },
        );
        self.analysis_dirty = true;
        if defer_analysis {
            Dispatch::none()
        } else {
            Dispatch::messages(self.flush_analysis())
        }
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
        if !self.accepts_replacement(&uri, text.len()) {
            let open = self.documents.get_mut(&uri).unwrap();
            let was_analyzed = !open.analysis_blocked;
            open.version = params.text_document.version;
            open.text.clear();
            open.hover = HoverIndex::default();
            open.completion = CompletionIndex::default();
            open.signature_help = SignatureHelpIndex::default();
            open.semantic_tokens = SemanticTokenIndex::default();
            open.definitions = DefinitionIndex::default();
            open.type_definitions = DefinitionIndex::default();
            open.document_symbols = DocumentSymbolIndex::default();
            open.folding_ranges = FoldingRangeIndex::default();
            open.diagnostics = analysis_limit_diagnostics();
            open.analysis_blocked = true;
            self.analysis_dirty |= was_analyzed;
            let mut messages = vec![publish_diagnostics(
                &uri,
                Some(params.text_document.version),
                &open.diagnostics,
            )];
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
        if defer_analysis {
            Dispatch::none()
        } else {
            Dispatch::messages(self.flush_analysis())
        }
    }

    fn did_close(&mut self, id: Option<Value>, params: Value, defer_analysis: bool) -> Dispatch {
        let Ok(params) = serde_json::from_value::<DidCloseParams>(params) else {
            return invalid_params(id);
        };
        let uri = params.text_document.uri;
        self.documents.remove(&uri);
        self.analysis_dirty = true;
        let mut messages = if defer_analysis {
            Vec::new()
        } else {
            self.flush_analysis()
        };
        messages.push(publish_diagnostics(&uri, None, &DiagnosticIndex::default()));
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
            json!({"isIncomplete": true, "items": items}),
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

    fn definition(&self, id: Option<Value>, params: Value) -> Dispatch {
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
            Value::Array(self.navigation_locations(&open.definitions, offset)),
        )])
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

    fn navigation_locations(&self, index: &DefinitionIndex, offset: u32) -> Vec<Value> {
        let targets = index.get(offset).collect::<Vec<_>>();
        if targets.is_empty() {
            return Vec::new();
        }
        let uris = self.analyzed_uris();
        let locations = targets
            .into_iter()
            .filter_map(|target| {
                let uri = uris.get(target.file as usize)?;
                let target_document = self.documents.get(*uri)?;
                Some(json!({
                    "uri": uri,
                    "range": {
                        "start": byte_offset_to_position(
                            &target_document.text,
                            target.span.lo as usize
                        ),
                        "end": byte_offset_to_position(
                            &target_document.text,
                            target.span.hi as usize
                        ),
                    }
                }))
            })
            .collect::<Vec<_>>();
        locations
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
                let document = self.documents.get(uri)?;
                Some(json!({
                    "uri": uri,
                    "range": {
                        "start": byte_offset_to_position(&document.text, span.lo as usize),
                        "end": byte_offset_to_position(&document.text, span.hi as usize),
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

    fn pull_diagnostics(&self, id: Option<Value>, params: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Ok(params) = serde_json::from_value::<DocumentDiagnosticParams>(params) else {
            return invalid_params(Some(id));
        };
        let items = self
            .documents
            .get(&params.text_document.uri)
            .map(|open| open.diagnostics.encode())
            .unwrap_or_default();
        let mut report = json!({"kind": "full"});
        report["items"] = Value::Array(items);
        Dispatch::messages(vec![rpc_result(id, report)])
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
            severity: Severity::Error,
            msg: format!(
                "workspace analysis limit exceeded (maximum {} MiB across {} open documents)",
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
    run_connection_with(reader, writer, super::super::analyze_for_lsp)
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
        Value::String(text) => 24usize.saturating_add(text.len()),
        Value::Array(values) => values.iter().fold(24usize, |total, value| {
            total.saturating_add(retained_value_bytes(value))
        }),
        Value::Object(values) => values.iter().fold(48usize, |total, (key, value)| {
            total
                .saturating_add(24)
                .saturating_add(key.len())
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

pub(super) fn dispatch_document_batch<W, A>(
    writer: &mut W,
    service: &mut LspService<A>,
    changes: Vec<Value>,
) -> io::Result<Option<i32>>
where
    W: Write,
    A: Analysis,
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
    loop {
        let event = match pending.pop_front() {
            Some(event) => event,
            None => match service.project_refresh_due_in() {
                Some(due) if due.is_zero() => {
                    for message in service.run_due_project_refresh() {
                        let encoded = serde_json::to_vec(&message).map_err(json_io)?;
                        write_framed(&mut writer, &encoded)?;
                    }
                    continue;
                }
                Some(due) => match incoming.recv_timeout(due) {
                    Ok(event) => event,
                    Err(RecvTimeoutError::Timeout) => continue,
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
                continue;
            }
            Incoming::Error(error) => return Err(error),
            Incoming::Eof => return Ok(0),
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
    }
}

fn json_io(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                severity: Severity::Error,
                msg: "same message".to_string(),
                file: 0,
            },
            Diagnostic {
                span: krusty::diag::Span::new(2, 3),
                severity: Severity::Warning,
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
            severity: Severity::Error,
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
                severity: Severity::Error,
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
                severity: Severity::Warning,
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
}
