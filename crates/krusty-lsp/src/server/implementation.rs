//! JSON-RPC/LSP session state and bounded stdio dispatch.
//!
//! This module lives in the separate `krusty-lsp` package, so the batch compiler neither links JSON
//! support nor retains server state. A session stores only the latest text and compact hover,
//! completion, and highlighting data for each open document; full compiler analysis is dropped after
//! every open/change notification.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::super::{
    CompletionIndex, DocumentAnalysis, HoverIndex, SemanticTokenIndex, SemanticTokenRange,
    SEMANTIC_TOKEN_MODIFIERS, SEMANTIC_TOKEN_TYPES,
};
use crate::worker::{source_set_fits, MAX_SOURCE_SET_BYTES};
use krusty::diag::{Diagnostic, Severity};

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_HEADER_BYTES: usize = 8 * 1024;
const INPUT_QUEUE_CAPACITY: usize = 4;
const MAX_OPEN_DOCUMENTS: usize = 256;
const MAX_BATCH_MESSAGES: usize = 256;
const MAX_BATCH_VALUE_BYTES: usize = 32 * 1024 * 1024;
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(150);
const MAX_BATCH_DURATION: Duration = Duration::from_millis(500);
const SERVER_VERSION: &str = match option_env!("KRUSTY_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Position {
    line: u32,
    character: u32,
}

/// Translate an LSP UTF-16 position into a source byte offset.
pub fn position_to_byte_offset(text: &str, target: Position) -> Option<u32> {
    let mut line = 0u32;
    let mut character = 0u32;
    let mut previous_was_cr = false;
    for (byte, ch) in text.char_indices() {
        if !(previous_was_cr && ch == '\n') && line == target.line && character == target.character
        {
            return u32::try_from(byte).ok();
        }
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

struct OpenDocument {
    text: String,
    version: i64,
    completion_generation: u64,
    hover: HoverIndex,
    completion: CompletionIndex,
    semantic_tokens: SemanticTokenIndex,
    analysis_blocked: bool,
}

/// Stateful LSP dispatcher with an injected analysis function for deterministic unit testing.
pub struct LspService<A> {
    documents: HashMap<String, OpenDocument>,
    analyze: A,
    analysis_dirty: bool,
    completion_generation: u64,
    initialized: bool,
    shutdown_requested: bool,
}

impl<A> LspService<A>
where
    A: FnMut(&[&str]) -> Vec<DocumentAnalysis>,
{
    pub fn new(analyze: A) -> Self {
        Self {
            documents: HashMap::new(),
            analyze,
            analysis_dirty: false,
            completion_generation: 0,
            initialized: false,
            shutdown_requested: false,
        }
    }

    pub fn open_document_count(&self) -> usize {
        self.documents.len()
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
        let mut uris: Vec<_> = self.documents.keys().cloned().collect();
        uris.retain(|uri| !self.documents[uri].analysis_blocked);
        uris.sort_unstable();
        let analyses = {
            let documents = &self.documents;
            let sources: Vec<_> = uris
                .iter()
                .map(|uri| documents[uri].text.as_str())
                .collect();
            (self.analyze)(&sources)
        };
        if analyses.len() != uris.len() {
            return uris
                .into_iter()
                .map(|uri| {
                    let open = &self.documents[&uri];
                    publish_diagnostics(
                        &uri,
                        Some(open.version),
                        vec![Diagnostic {
                            span: krusty::diag::Span::new(0, 0),
                            severity: Severity::Error,
                            msg: "analysis worker returned an incomplete source set".to_string(),
                            file: 0,
                        }],
                        &open.text,
                    )
                })
                .collect();
        }
        self.completion_generation = self.completion_generation.wrapping_add(1);
        let completion_generation = self.completion_generation;

        uris.into_iter()
            .zip(analyses)
            .map(|(uri, analysis)| {
                let open = self.documents.get_mut(&uri).unwrap();
                open.completion_generation = completion_generation;
                open.hover = analysis.hover;
                open.completion = analysis.completion;
                open.semantic_tokens = analysis.semantic_tokens;
                publish_diagnostics(&uri, Some(open.version), analysis.diagnostics, &open.text)
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
                Dispatch::messages(vec![rpc_result(
                    id,
                    json!({
                        "capabilities": {
                            "hoverProvider": true,
                            "completionProvider": {
                                "resolveProvider": true,
                                "triggerCharacters": ["."],
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
                            "textDocumentSync": 1
                        },
                        "serverInfo": {
                            "name": "krusty-lsp",
                            "version": SERVER_VERSION
                        }
                    }),
                )])
            }
            "initialized" => Dispatch::none(),
            "textDocument/didOpen" => self.did_open(id, params, defer_analysis),
            "textDocument/didChange" => self.did_change(id, params, defer_analysis),
            "textDocument/didClose" => self.did_close(id, params, defer_analysis),
            "textDocument/hover" => self.hover(id, params),
            "textDocument/completion" => self.completion(id, params),
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
                        completion_generation: 0,
                        hover: HoverIndex::default(),
                        completion: CompletionIndex::default(),
                        semantic_tokens: SemanticTokenIndex::default(),
                        analysis_blocked: true,
                    },
                );
            }
            self.analysis_dirty |= replaced_analyzed_document;
            let mut messages = vec![analysis_limit_diagnostic(
                &uri,
                version,
                &params.text_document.text,
            )];
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
                completion_generation: 0,
                hover: HoverIndex::default(),
                completion: CompletionIndex::default(),
                semantic_tokens: SemanticTokenIndex::default(),
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
        if params.content_changes.len() != 1 || params.content_changes[0].range.is_some() {
            return invalid_params(id);
        }
        let uri = params.text_document.uri;
        let Some(open) = self.documents.get(&uri) else {
            return invalid_params(id);
        };
        if params.text_document.version <= open.version {
            return Dispatch::none();
        }
        let text = params.content_changes.pop().unwrap().text;
        if !self.accepts_replacement(&uri, text.len()) {
            let open = self.documents.get_mut(&uri).unwrap();
            let was_analyzed = !open.analysis_blocked;
            open.version = params.text_document.version;
            open.text.clear();
            open.hover = HoverIndex::default();
            open.completion = CompletionIndex::default();
            open.semantic_tokens = SemanticTokenIndex::default();
            open.analysis_blocked = true;
            self.analysis_dirty |= was_analyzed;
            let mut messages = vec![analysis_limit_diagnostic(
                &uri,
                params.text_document.version,
                &text,
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
        messages.push(publish_diagnostics(&uri, None, Vec::new(), ""));
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
        Dispatch::messages(vec![rpc_result(
            id,
            json!({
                "contents": {"kind": "plaintext", "value": hover.type_name},
                "range": {
                    "start": byte_offset_to_position(&open.text, hover.span.lo as usize),
                    "end": byte_offset_to_position(&open.text, hover.span.hi as usize),
                }
            }),
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
        let is_incomplete = open.completion.is_incomplete();
        let items: Vec<_> = open
            .completion
            .complete(&open.text, offset)
            .into_iter()
            .map(|candidate| {
                json!({
                    "label": candidate.label,
                    "kind": candidate.kind,
                    "data": {
                        "uri": params.text_document.uri,
                        "version": open.version,
                        "generation": open.completion_generation,
                        "slot": candidate.slot,
                    }
                })
            })
            .collect();
        Dispatch::messages(vec![rpc_result(
            id,
            json!({"isIncomplete": is_incomplete, "items": items}),
        )])
    }

    fn resolve_completion(&self, id: Option<Value>, mut item: Value) -> Dispatch {
        let Some(id) = id else {
            return Dispatch::none();
        };
        let Some(object) = item.as_object_mut() else {
            return invalid_params(Some(id));
        };
        let Some(label) = object
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return invalid_params(Some(id));
        };
        let Some(data) = object.get("data") else {
            return Dispatch::messages(vec![rpc_result(id, item)]);
        };
        let Some(uri) = data.get("uri").and_then(Value::as_str) else {
            return Dispatch::messages(vec![rpc_result(id, item)]);
        };
        let Some(version) = data.get("version").and_then(Value::as_i64) else {
            return Dispatch::messages(vec![rpc_result(id, item)]);
        };
        let Some(generation) = data.get("generation").and_then(Value::as_u64) else {
            return Dispatch::messages(vec![rpc_result(id, item)]);
        };
        let Some(slot) = data
            .get("slot")
            .and_then(Value::as_u64)
            .and_then(|slot| u32::try_from(slot).ok())
        else {
            return Dispatch::messages(vec![rpc_result(id, item)]);
        };
        if let Some(detail) = self
            .documents
            .get(uri)
            .filter(|document| {
                document.version == version && document.completion_generation == generation
            })
            .and_then(|document| document.completion.resolve(slot, &label))
        {
            object.insert("detail".to_string(), Value::String(detail.to_string()));
        }
        Dispatch::messages(vec![rpc_result(id, item)])
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
struct ContentChange {
    text: String,
    #[serde(default)]
    range: Option<Value>,
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
struct DidCloseParams {
    text_document: TextDocumentIdentifier,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextDocumentPositionParams {
    text_document: TextDocumentIdentifier,
    position: Position,
}

#[derive(Clone, Copy, Deserialize)]
struct Range {
    start: Position,
    end: Position,
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
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

fn publish_diagnostics(
    uri: &str,
    version: Option<i64>,
    diagnostics: Vec<Diagnostic>,
    text: &str,
) -> Value {
    let diagnostics: Vec<Value> = diagnostics
        .into_iter()
        .map(|diagnostic| {
            let start = diagnostic.span.lo as usize;
            let end = usize::max(start, diagnostic.span.hi as usize);
            json!({
                "range": {
                    "start": byte_offset_to_position(text, start),
                    "end": byte_offset_to_position(text, end),
                },
                "severity": match diagnostic.severity {
                    Severity::Error => 1,
                    Severity::Warning => 2,
                },
                "source": "Kotlin",
                "message": lsp_diagnostic_message(diagnostic.msg),
            })
        })
        .collect();
    let mut params = json!({"uri": uri, "diagnostics": diagnostics});
    if let Some(version) = version {
        params["version"] = json!(version);
    }
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": params,
    })
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

fn analysis_limit_diagnostic(uri: &str, version: i64, text: &str) -> Value {
    publish_diagnostics(
        uri,
        Some(version),
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
        text,
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
    A: FnMut(&[&str]) -> Vec<DocumentAnalysis>,
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
                let replace = next_change.as_ref().and_then(|(next_uri, next_version)| {
                    changes
                        .iter()
                        .rposition(|change| {
                            document_notification_identity(change)
                                .is_some_and(|(_, uri)| uri == next_uri)
                        })
                        .filter(|&index| change_identity(&changes[index]).is_some())
                        .map(|index| (index, *next_version))
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
    A: FnMut(&[&str]) -> Vec<DocumentAnalysis>,
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
    A: FnMut(&[&str]) -> Vec<DocumentAnalysis>,
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
            None => incoming.recv().unwrap_or(Incoming::Eof),
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
