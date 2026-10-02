//! Bounded diagnostic payloads.
//!
//! A workspace report that fits in one [`RESPONSE_PAGE_BYTES`] frame is the JSON-RPC result.
//! A client that sends a `partialResultToken` and a report that does not fit receives every page
//! through `$/progress`, and the final result is an empty item list. That stream fits in the
//! stdout channel, including the empty final result. A longer report, including one file that
//! cannot fit a frame, is a single server-cancelled error and no `$/progress`. The item list is the snapshot taken for the
//! request; the response is not revised when later input arrives. A refresh is not used as a
//! cursor for the omitted files.

use serde_json::{json, Value};

use super::response_page::{
    json_len, limit_text, paged_array_messages, server_cancelled, RESPONSE_PAGE_BYTES,
};

/// Room for the workspace-report envelope (`uri`, `resultId`, `kind`) around one file's items,
/// so a single file still occupies one page.
const DIAGNOSTIC_REPORT_OVERHEAD_BYTES: usize = 4 * 1024;

/// Diagnostic items inside one file. The workspace page budget includes the report envelope.
pub(super) const DIAGNOSTIC_ITEMS_PAGE_BYTES: usize =
    RESPONSE_PAGE_BYTES - DIAGNOSTIC_REPORT_OVERHEAD_BYTES;

/// One diagnostic's `message` on the wire. The analysis store may retain a longer compiler
/// message; the editor payload does not.
pub(super) const DIAGNOSTIC_MESSAGE_WIRE_BYTES: usize = 8 * 1024;

pub(super) const DIAGNOSTIC_OMISSION_MESSAGE: &str =
    "Additional diagnostics omitted (response page limit).";

pub(super) fn wire_diagnostic_message(message: &str) -> std::borrow::Cow<'_, str> {
    limit_text(message, DIAGNOSTIC_MESSAGE_WIRE_BYTES)
}

pub(super) fn limit_diagnostic_items(items: Vec<Value>) -> Vec<Value> {
    fit_json_array(items, DIAGNOSTIC_ITEMS_PAGE_BYTES, true)
}

pub(super) fn workspace_diagnostic_messages(
    id: Value,
    items: Vec<Value>,
    token: Option<&Value>,
) -> Vec<Value> {
    paged_array_messages(id, items, token, result_report, progress_report, too_large)
}

fn result_report(id: &Value, items: Vec<Value>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "items": items }
    })
}

fn progress_report(token: &Value, items: Vec<Value>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": {
            "token": token,
            "value": { "items": items }
        }
    })
}

fn too_large(id: &Value) -> Value {
    server_cancelled(
        id,
        "workspace diagnostic report exceeds the bounded non-streaming response limit",
        Some(json!({"retriggerRequest": false})),
    )
}

fn omission_diagnostic() -> Value {
    json!({
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 0}
        },
        "severity": 1,
        "source": "Kotlin",
        "message": DIAGNOSTIC_OMISSION_MESSAGE,
    })
}

/// Keep a prefix of `items` whose JSON array fits in `budget`. When `mark_omission` is set and
/// some items do not fit, the prefix ends with a diagnostic that says so.
fn fit_json_array(items: Vec<Value>, budget: usize, mark_omission: bool) -> Vec<Value> {
    let lengths: Vec<usize> = items.iter().map(json_len).collect();
    let mut count = 0usize;
    while count < lengths.len() && json_array_len(&lengths[..=count]) <= budget {
        count += 1;
    }
    if count == items.len() || !mark_omission {
        return items.into_iter().take(count).collect();
    }

    let marker = omission_diagnostic();
    let marker_len = json_len(&marker);
    while count > 0 && json_array_len_with_marker(&lengths[..count], marker_len) > budget {
        count -= 1;
    }
    if json_array_len_with_marker(&lengths[..count], marker_len) > budget {
        return Vec::new();
    }
    let mut kept: Vec<Value> = items.into_iter().take(count).collect();
    kept.push(marker);
    kept
}

fn json_array_len(item_lens: &[usize]) -> usize {
    if item_lens.is_empty() {
        return 2;
    }
    let separators = item_lens.len() - 1;
    let body = item_lens
        .iter()
        .fold(separators, |sum, len| sum.saturating_add(*len));
    body.saturating_add(2)
}

fn json_array_len_with_marker(item_lens: &[usize], marker_len: usize) -> usize {
    let mut lengths = Vec::with_capacity(item_lens.len().saturating_add(1));
    lengths.extend_from_slice(item_lens);
    lengths.push(marker_len);
    json_array_len(&lengths)
}

#[cfg(test)]
mod tests {
    use super::super::response_page::MAX_RESPONSE_PAGES;
    use super::*;

    fn full_item(uri: &str, message: &str) -> Value {
        json!({
            "kind": "full",
            "uri": uri,
            "version": Value::Null,
            "resultId": "0123456789abcdef",
            "items": [{
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 1}
                },
                "severity": 1,
                "source": "Kotlin",
                "message": message
            }]
        })
    }

    fn unchanged_item(uri: &str) -> Value {
        json!({
            "kind": "unchanged",
            "uri": uri,
            "version": Value::Null,
            "resultId": "0123456789abcdef"
        })
    }

    fn char_boundary_cut(text: &str, max_bytes: usize) -> String {
        let ellipsis = "…";
        let mut end = max_bytes.saturating_sub(ellipsis.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut truncated = text[..end].to_string();
        truncated.push_str(ellipsis);
        truncated
    }

    #[test]
    fn a_long_diagnostic_message_is_cut_on_a_char_boundary() {
        let message = "é".repeat(DIAGNOSTIC_MESSAGE_WIRE_BYTES);
        let expected = char_boundary_cut(&message, DIAGNOSTIC_MESSAGE_WIRE_BYTES);
        assert_eq!(expected.len(), DIAGNOSTIC_MESSAGE_WIRE_BYTES - 1);
        assert_eq!(wire_diagnostic_message(&message).as_ref(), expected);
        assert_eq!(wire_diagnostic_message("short").as_ref(), "short");
    }

    fn array_fits(items: &[Value], budget: usize) -> bool {
        serde_json::to_vec(&Value::Array(items.to_vec()))
            .unwrap()
            .len()
            <= budget
    }

    fn items_with_omission(items: &[Value], budget: usize) -> Vec<Value> {
        let mut count = 0usize;
        while count < items.len() && array_fits(&items[..=count], budget) {
            count += 1;
        }
        if count == items.len() {
            return items.to_vec();
        }
        let marker = omission_diagnostic();
        while count > 0 {
            let mut kept = items[..count].to_vec();
            kept.push(marker.clone());
            if array_fits(&kept, budget) {
                return kept;
            }
            count -= 1;
        }
        Vec::new()
    }

    #[test]
    fn diagnostic_items_stop_at_the_page_and_say_so() {
        let item = json!({"message": "m".repeat(1024)});
        let items = vec![item; 400];
        let expected = items_with_omission(&items, DIAGNOSTIC_ITEMS_PAGE_BYTES);
        assert_ne!(expected.len(), 400);
        assert_eq!(
            expected.last().unwrap()["message"],
            DIAGNOSTIC_OMISSION_MESSAGE
        );
        assert_eq!(limit_diagnostic_items(items), expected);
    }

    #[test]
    fn partial_result_token_accepts_string_and_integer_only() {
        use super::super::response_page::partial_result_token;

        assert_eq!(partial_result_token(&json!({})).unwrap(), None);
        assert_eq!(
            partial_result_token(&json!({"partialResultToken": null})).unwrap(),
            None
        );
        assert_eq!(
            partial_result_token(&json!({"partialResultToken": "workspace/diagnostic/1"})).unwrap(),
            Some(json!("workspace/diagnostic/1"))
        );
        assert_eq!(
            partial_result_token(&json!({"partialResultToken": 7})).unwrap(),
            Some(json!(7))
        );
        assert!(partial_result_token(&json!({"partialResultToken": true})).is_err());
        assert!(partial_result_token(&json!({"partialResultToken": {"t": 1}})).is_err());
        assert!(partial_result_token(&json!({"partialResultToken": 1.5})).is_err());
    }

    #[test]
    fn a_token_streams_every_report_and_the_final_result_is_empty() {
        let message = "m".repeat(8 * 1024);
        let items: Vec<Value> = (0..80)
            .map(|index| full_item(&format!("file:///w/F{index:03}.kt"), &message))
            .collect();
        let token = json!("workspace/diagnostic/1");
        let messages = workspace_diagnostic_messages(json!(2), items.clone(), Some(&token));
        let budget = progress_budget(&token);
        let mut expected = Vec::new();
        let mut page = Vec::new();
        let mut used = 2usize;
        for item in &items {
            let len = json_len(item);
            let separator = usize::from(!page.is_empty());
            if used.saturating_add(separator).saturating_add(len) > budget {
                expected.push(json!({
                    "jsonrpc": "2.0",
                    "method": "$/progress",
                    "params": {"token": token, "value": {"items": page}}
                }));
                page = Vec::new();
                used = 2;
            }
            let separator = usize::from(!page.is_empty());
            used = used.saturating_add(separator).saturating_add(len);
            page.push(item.clone());
        }
        expected.push(json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {"token": token, "value": {"items": page}}
        }));
        expected.push(json!({"jsonrpc": "2.0", "id": 2, "result": {"items": []}}));
        assert_eq!(messages, expected);
    }

    #[test]
    fn a_report_that_fits_stays_in_the_result() {
        let items = vec![unchanged_item("file:///w/One.kt")];
        let messages = workspace_diagnostic_messages(json!(1), items.clone(), Some(&json!("tok")));
        assert_eq!(
            messages,
            vec![json!({"jsonrpc": "2.0", "id": 1, "result": {"items": items}})]
        );
    }

    fn server_cancelled(id: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32802,
                "message": "workspace diagnostic report exceeds the bounded non-streaming response limit",
                "data": {"retriggerRequest": false}
            }
        })
    }

    #[test]
    fn without_a_token_an_oversized_report_is_server_cancelled() {
        let message = "m".repeat(8 * 1024);
        let items: Vec<Value> = (0..80)
            .map(|index| full_item(&format!("file:///w/F{index:03}.kt"), &message))
            .collect();
        let messages = workspace_diagnostic_messages(json!(3), items, None);
        assert_eq!(messages, vec![server_cancelled(json!(3))]);
    }

    #[test]
    fn one_report_that_does_not_fit_a_frame_is_not_sent() {
        let huge = full_item("file:///w/Huge.kt", &"m".repeat(RESPONSE_PAGE_BYTES));
        let messages = workspace_diagnostic_messages(json!(1), vec![huge], Some(&json!("tok")));
        assert_eq!(messages, vec![server_cancelled(json!(1))]);
    }

    #[test]
    fn a_large_request_id_is_charged_in_the_frame() {
        let id = Value::String("i".repeat(RESPONSE_PAGE_BYTES));
        let messages =
            workspace_diagnostic_messages(id.clone(), vec![unchanged_item("file:///w/A.kt")], None);
        assert_eq!(messages, vec![server_cancelled(id)]);
    }

    #[test]
    fn workspace_diagnostics_stream_on_the_session_and_fail_closed_without_a_token() {
        use super::super::implementation::LspService;
        use crate::{DocumentAnalysis, IndexedFile};
        use krusty::diag::{Diagnostic, DiagnosticKind, Severity};

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
                    "textDocument": { "diagnostic": {} },
                    "workspace": { "diagnostics": { "refreshSupport": true } }
                }
            }
        }));
        let message = "m".repeat(8 * 1024);
        let files: Vec<IndexedFile> = (0..80)
            .map(|index| IndexedFile {
                uri: format!("file:///w/F{index:03}.kt"),
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 1),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: message.clone(),
                    file: 0,
                }],
                text_hash: 1,
                text: "x".into(),
            })
            .collect();
        let uris: Vec<String> = files.iter().map(|file| file.uri.clone()).collect();
        service.apply_index_batch(super::super::engine::IndexBatch {
            generation: 0,
            attempted: uris.clone(),
            conclusive: true,
            files,
        });

        let streamed = service.handle(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "workspace/diagnostic",
            "params": { "partialResultToken": "workspace/diagnostic/2" }
        }));
        let result_id = streamed.messages[0]["params"]["value"]["items"][0]["resultId"]
            .as_str()
            .expect("result id");
        assert!(
            result_id.len() == 16
                && result_id
                    .chars()
                    .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
            "{result_id}"
        );
        let expected = eighty_file_stream(2, "workspace/diagnostic/2", result_id);
        assert_eq!(streamed.messages, expected);
        let encoded: Vec<usize> = streamed
            .messages
            .iter()
            .map(|message| serde_json::to_vec(message).unwrap().len())
            .collect();
        assert_eq!(encoded, vec![260_782, 260_782, 151_465, 46]);
        let seen: Vec<String> = expected[..expected.len() - 1]
            .iter()
            .flat_map(|message| {
                message["params"]["value"]["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["uri"].as_str().unwrap().to_string())
            })
            .collect();
        assert_eq!(seen, uris);

        let refused = service.handle(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "workspace/diagnostic",
            "params": {}
        }));
        assert_eq!(refused.messages, vec![server_cancelled(json!(3))]);

        let invalid = service.handle(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "workspace/diagnostic",
            "params": { "partialResultToken": true }
        }));
        assert_eq!(
            invalid.messages,
            vec![json!({
                "jsonrpc": "2.0",
                "id": 4,
                "error": {"code": -32602, "message": "invalid params"}
            })]
        );
    }

    fn eighty_file_stream(id: i64, token: &str, result_id: &str) -> Vec<Value> {
        let wire = format!("M{}", "m".repeat(8 * 1024 - 1));
        let mut expected = Vec::new();
        for range in [0..31, 31..62, 62..80] {
            let page: Vec<Value> = range
                .map(|index| {
                    json!({
                        "kind": "full",
                        "uri": format!("file:///w/F{index:03}.kt"),
                        "version": Value::Null,
                        "resultId": result_id,
                        "items": [{
                            "range": {
                                "start": {"line": 0, "character": 0},
                                "end": {"line": 0, "character": 1}
                            },
                            "severity": 1,
                            "source": "Kotlin",
                            "message": wire,
                        }]
                    })
                })
                .collect();
            expected.push(json!({
                "jsonrpc": "2.0",
                "method": "$/progress",
                "params": {"token": token, "value": {"items": page}}
            }));
        }
        expected.push(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"items": []}
        }));
        expected
    }

    #[test]
    fn a_queued_cancel_and_index_reset_leave_the_snapshot_intact() {
        use std::collections::VecDeque;
        use std::sync::mpsc::sync_channel;

        use super::super::engine::EngineEvent;
        use super::super::implementation::{step_async, Incoming, LspService};
        use crate::{DocumentAnalysis, IndexedFile};
        use krusty::diag::{Diagnostic, DiagnosticKind, Severity};

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
                    "textDocument": { "diagnostic": {} },
                    "workspace": { "diagnostics": { "refreshSupport": true } }
                }
            }
        }));
        let message = "m".repeat(8 * 1024);
        let files: Vec<IndexedFile> = (0..80)
            .map(|index| IndexedFile {
                uri: format!("file:///w/F{index:03}.kt"),
                diagnostics: vec![Diagnostic {
                    span: krusty::diag::Span::new(0, 1),
                    editor_span: None,
                    identity: None,
                    severity: Severity::Error,
                    kind: DiagnosticKind::Compiler,
                    msg: message.clone(),
                    file: 0,
                }],
                text_hash: 1,
                text: "x".into(),
            })
            .collect();
        let uris: Vec<String> = files.iter().map(|file| file.uri.clone()).collect();
        service.apply_index_batch(super::super::engine::IndexBatch {
            generation: 0,
            attempted: uris,
            conclusive: true,
            files,
        });
        let request = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "workspace/diagnostic",
            "params": { "partialResultToken": "workspace/diagnostic/2" }
        });
        let snapshot = service.handle(request.clone());
        let result_id = snapshot.messages[0]["params"]["value"]["items"][0]["resultId"]
            .as_str()
            .expect("result id");
        let expected = eighty_file_stream(2, "workspace/diagnostic/2", result_id);
        assert_eq!(snapshot.messages, expected);

        let cancel = json!({
            "jsonrpc": "2.0",
            "method": "$/cancelRequest",
            "params": {"id": 2}
        });
        let (tx, incoming) = sync_channel(4);
        tx.send(Incoming::Message(cancel.clone())).unwrap();
        tx.send(Incoming::Engine(EngineEvent::IndexReset(1)))
            .unwrap();
        let mut pending = VecDeque::new();
        let mut written = Vec::new();
        let exit = step_async(
            &mut service,
            &mut written,
            &incoming,
            &mut pending,
            Incoming::Message(request),
        )
        .unwrap();
        assert_eq!(exit, None);
        assert!(pending.is_empty());
        assert_eq!(decode_frames(&written), expected);
        match incoming.try_recv().expect("cancel still queued") {
            Incoming::Message(message) => assert_eq!(message, cancel),
            Incoming::ParseError | Incoming::Error(_) | Incoming::Eof => {
                panic!("the queued cancel was not left unread")
            }
            Incoming::Engine(_) => panic!("the index reset was taken before the cancel"),
        }
        match incoming.try_recv().expect("index reset still queued") {
            Incoming::Engine(EngineEvent::IndexReset(generation)) => assert_eq!(generation, 1),
            Incoming::Engine(_) => panic!("a different engine event was queued"),
            Incoming::Message(_) | Incoming::ParseError | Incoming::Error(_) | Incoming::Eof => {
                panic!("the index reset was not left unread")
            }
        }
    }

    #[test]
    fn one_file_diagnostic_list_stays_inside_a_single_page() {
        use super::super::implementation::LspService;
        use crate::server::AnalysisBatch;
        use crate::DocumentAnalysis;
        use krusty::diag::{Diagnostic, DiagnosticKind, Severity};

        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        service.open_document_for_test("file:///a.kt", "bad", 1);
        let diagnostics = (0..400)
            .map(|index| Diagnostic {
                span: krusty::diag::Span::new(0, 1),
                editor_span: None,
                identity: None,
                severity: Severity::Error,
                kind: DiagnosticKind::Compiler,
                msg: format!("error {index} {}", "m".repeat(1024)),
                file: 0,
            })
            .collect();
        let messages = service.apply_analysis_batch(AnalysisBatch {
            analyzed: vec![("file:///a.kt".into(), 1)],
            analyses: vec![DocumentAnalysis {
                diagnostics,
                ..DocumentAnalysis::empty()
            }],
            support_documents: Vec::new(),
            pending: false,
        });
        let published = messages
            .iter()
            .find(|message| message["method"] == "textDocument/publishDiagnostics")
            .expect("push diagnostics");
        let wire: Vec<Value> = (0..400)
            .map(|index| {
                json!({
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 1}
                    },
                    "severity": 1,
                    "source": "Kotlin",
                    "message": format!("Error {index} {}", "m".repeat(1024))
                })
            })
            .collect();
        let expected = items_with_omission(&wire, DIAGNOSTIC_ITEMS_PAGE_BYTES);
        assert_ne!(expected.len(), 400);
        assert_eq!(published["params"]["diagnostics"], Value::Array(expected));
        assert_eq!(published["params"]["uri"], "file:///a.kt");
        assert_eq!(published["params"]["version"], 1);
    }

    fn progress_budget(token: &Value) -> usize {
        let shell = json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {"token": token, "value": {"items": []}}
        });
        RESPONSE_PAGE_BYTES - json_len(&shell) + 2
    }

    fn page_filling_message(token: &Value) -> String {
        let budget = progress_budget(token);
        let mut low = 0usize;
        let mut high = budget;
        let mut message = String::new();
        while low <= high {
            let mid = low + (high - low) / 2;
            let candidate = "m".repeat(mid);
            let item = full_item("file:///w/F000.kt", &candidate);
            if json_len(&item).saturating_add(2) <= budget {
                message = candidate;
                low = mid.saturating_add(1);
            } else if mid == 0 {
                break;
            } else {
                high = mid - 1;
            }
        }
        let item = full_item("file:///w/F000.kt", &message);
        let len = json_len(&item);
        assert!(len.saturating_add(2) <= budget);
        assert!(len.saturating_add(1).saturating_add(len).saturating_add(2) > budget);
        message
    }

    fn page_filling_items(count: usize, token: &Value) -> Vec<Value> {
        let message = page_filling_message(token);
        (0..count)
            .map(|index| full_item(&format!("file:///w/F{index:03}.kt"), &message))
            .collect()
    }

    #[test]
    fn a_report_past_the_channel_is_server_cancelled_with_no_progress() {
        let token = json!("workspace/diagnostic/9");
        let items = page_filling_items(MAX_RESPONSE_PAGES + 1, &token);
        let messages = workspace_diagnostic_messages(json!(9), items, Some(&token));
        assert_eq!(messages, vec![server_cancelled(json!(9))]);
        assert_eq!(deliver_blocked(&messages), messages);
    }

    #[test]
    fn a_report_that_fills_the_channel_keeps_its_final_result() {
        let token = json!("workspace/diagnostic/9");
        let items = page_filling_items(MAX_RESPONSE_PAGES, &token);
        let messages = workspace_diagnostic_messages(json!(9), items.clone(), Some(&token));
        assert_eq!(messages.len(), MAX_RESPONSE_PAGES + 1);
        for (index, message) in messages.iter().take(MAX_RESPONSE_PAGES).enumerate() {
            assert_eq!(
                message,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "$/progress",
                    "params": {
                        "token": token,
                        "value": {"items": [items[index]]}
                    }
                })
            );
        }
        assert_eq!(
            messages[MAX_RESPONSE_PAGES],
            json!({"jsonrpc": "2.0", "id": 9, "result": {"items": []}})
        );
    }

    #[test]
    fn a_channel_sized_report_is_delivered_through_a_blocked_stdout_queue() {
        let token = json!("workspace/diagnostic/9");
        let items = page_filling_items(MAX_RESPONSE_PAGES, &token);
        let messages = workspace_diagnostic_messages(json!(9), items, Some(&token));
        assert_eq!(messages.len(), MAX_RESPONSE_PAGES + 1);
        assert_eq!(deliver_blocked(&messages), messages);
    }

    fn decode_frames(bytes: &[u8]) -> Vec<Value> {
        let mut decoded = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let header_end = rest
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("framed header");
            let header = std::str::from_utf8(&rest[..header_end]).unwrap();
            let length: usize = header
                .strip_prefix("Content-Length: ")
                .unwrap()
                .parse()
                .unwrap();
            let body_start = header_end + 4;
            let body = &rest[body_start..body_start + length];
            decoded.push(serde_json::from_slice::<Value>(body).unwrap());
            rest = &rest[body_start + length..];
        }
        decoded
    }

    fn deliver_blocked(messages: &[Value]) -> Vec<Value> {
        use std::io::Write;
        use std::sync::{Arc, Condvar, Mutex};
        use std::time::{Duration, Instant};

        use super::super::implementation::write_framed;
        use super::super::output_queue::OutputQueue;

        struct GatedWriter {
            started: Arc<Mutex<usize>>,
            gate: Arc<(Mutex<bool>, Condvar)>,
            captured: Arc<Mutex<Vec<u8>>>,
        }

        struct OpenGate(Arc<(Mutex<bool>, Condvar)>);
        impl Drop for OpenGate {
            fn drop(&mut self) {
                let (lock, cv) = &*self.0;
                *lock.lock().expect("open") = true;
                cv.notify_all();
            }
        }

        impl Write for GatedWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                *self.started.lock().expect("started") += 1;
                let (lock, cv) = &*self.gate;
                let mut open = lock.lock().expect("gate");
                while !*open {
                    open = cv.wait(open).expect("gate");
                }
                self.captured
                    .lock()
                    .expect("captured")
                    .extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let started = Arc::new(Mutex::new(0usize));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let mut queue = OutputQueue::spawn(GatedWriter {
            started: Arc::clone(&started),
            gate: Arc::clone(&gate),
            captured: Arc::clone(&captured),
        })
        .unwrap();
        let _release = OpenGate(Arc::clone(&gate));
        for message in messages {
            let encoded = serde_json::to_vec(message).unwrap();
            write_framed(&mut queue, &encoded).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while *started.lock().expect("started") == 0 {
            assert!(Instant::now() < deadline, "writer thread did not start");
            std::thread::sleep(Duration::from_millis(1));
        }
        {
            let (lock, cv) = &*gate;
            *lock.lock().expect("open") = true;
            cv.notify_all();
        }
        drop(queue);
        let bytes = captured.lock().expect("captured").clone();
        decode_frames(&bytes)
    }
}
