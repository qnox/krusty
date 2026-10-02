//! Bounded diagnostic payloads.
//!
//! A workspace report that fits in one [`RESPONSE_PAGE_BYTES`] frame is the JSON-RPC result.
//! A client that sends a `partialResultToken` and a report that does not fit receives every page
//! through `$/progress`, and the final result is an empty item list. A report that still does
//! not fit, including one file that cannot fit a frame, is server-cancelled. A refresh is not
//! used as a cursor for the omitted files.

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

    #[test]
    fn a_long_diagnostic_message_is_cut_on_a_char_boundary() {
        let message = "é".repeat(DIAGNOSTIC_MESSAGE_WIRE_BYTES);
        let wire = wire_diagnostic_message(&message);
        assert!(wire.len() <= DIAGNOSTIC_MESSAGE_WIRE_BYTES);
        assert!(wire.ends_with('…'));
        assert!(wire.starts_with('é'));
        assert_eq!(wire_diagnostic_message("short").as_ref(), "short");
    }

    #[test]
    fn diagnostic_items_stop_at_the_page_and_say_so() {
        let item = json!({"message": "m".repeat(1024)});
        let items = vec![item; 400];
        let limited = limit_diagnostic_items(items);
        let encoded = serde_json::to_vec(&Value::Array(limited.clone())).unwrap();
        assert!(encoded.len() <= DIAGNOSTIC_ITEMS_PAGE_BYTES);
        assert!(limited.len() < 400);
        assert_eq!(
            limited.last().unwrap()["message"],
            DIAGNOSTIC_OMISSION_MESSAGE
        );
    }

    fn assert_frames_fit(messages: &[Value]) {
        for message in messages {
            let encoded = serde_json::to_vec(message).unwrap();
            assert!(
                encoded.len() <= RESPONSE_PAGE_BYTES,
                "frame is {} bytes",
                encoded.len()
            );
        }
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
        let messages = workspace_diagnostic_messages(json!(2), items, Some(&token));
        assert!(messages.len() > 2);
        assert_frames_fit(&messages);
        assert_eq!(messages.last().unwrap()["result"], json!({"items": []}));
        let mut seen = Vec::new();
        for message in &messages[..messages.len() - 1] {
            assert_eq!(message["method"], "$/progress");
            assert_eq!(message["params"]["token"], token);
            let page = message["params"]["value"]["items"].as_array().unwrap();
            assert!(!page.is_empty());
            seen.extend(
                page.iter()
                    .map(|item| item["uri"].as_str().unwrap().to_string()),
            );
        }
        let expected: Vec<String> = (0..80)
            .map(|index| format!("file:///w/F{index:03}.kt"))
            .collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn a_report_that_fits_stays_in_the_result() {
        let items = vec![unchanged_item("file:///w/One.kt")];
        let messages = workspace_diagnostic_messages(json!(1), items.clone(), Some(&json!("tok")));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["result"]["items"], Value::Array(items));
    }

    #[test]
    fn without_a_token_an_oversized_report_is_server_cancelled() {
        let message = "m".repeat(8 * 1024);
        let items: Vec<Value> = (0..80)
            .map(|index| full_item(&format!("file:///w/F{index:03}.kt"), &message))
            .collect();
        let messages = workspace_diagnostic_messages(json!(3), items, None);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["error"]["code"], -32802);
        assert_eq!(messages[0]["error"]["data"]["retriggerRequest"], false);
        assert!(messages[0].get("result").is_none());
        assert!(messages[0].get("method").is_none());
    }

    #[test]
    fn one_report_that_does_not_fit_a_frame_is_not_sent() {
        let huge = full_item("file:///w/Huge.kt", &"m".repeat(RESPONSE_PAGE_BYTES));
        let messages = workspace_diagnostic_messages(json!(1), vec![huge], Some(&json!("tok")));
        assert_eq!(messages[0]["error"]["code"], -32802);
        let encoded = serde_json::to_vec(&messages[0]).unwrap();
        assert!(!encoded.windows(4).any(|window| window == b"Huge"));
    }

    #[test]
    fn a_large_request_id_is_charged_in_the_frame() {
        let id = Value::String("i".repeat(RESPONSE_PAGE_BYTES));
        let messages =
            workspace_diagnostic_messages(id, vec![unchanged_item("file:///w/A.kt")], None);
        assert_eq!(messages[0]["error"]["code"], -32802);
        assert!(
            serde_json::to_vec(&messages[0]).unwrap().len()
                <= super::super::implementation::MAX_MESSAGE_BYTES
        );
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
        assert!(streamed.messages.len() > 2);
        assert_frames_fit(&streamed.messages);
        assert_eq!(
            streamed.messages.last().unwrap()["result"],
            json!({"items": []})
        );
        let mut seen = Vec::new();
        for message in &streamed.messages[..streamed.messages.len() - 1] {
            assert_eq!(message["method"], "$/progress");
            assert_eq!(message["params"]["token"], "workspace/diagnostic/2");
            seen.extend(
                message["params"]["value"]["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["uri"].as_str().unwrap().to_string()),
            );
        }
        assert_eq!(seen, uris);

        let refused = service.handle(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "workspace/diagnostic",
            "params": {}
        }));
        assert_eq!(refused.messages.len(), 1);
        assert_eq!(refused.messages[0]["error"]["code"], -32802);
        assert!(refused
            .messages
            .iter()
            .all(|message| message["method"] != "workspace/diagnostic/refresh"));

        let invalid = service.handle(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "workspace/diagnostic",
            "params": { "partialResultToken": true }
        }));
        assert_eq!(invalid.messages[0]["error"]["code"], -32602);
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
        let items = published["params"]["diagnostics"]
            .as_array()
            .expect("diagnostic list");
        let encoded = serde_json::to_vec(items).unwrap();
        assert!(encoded.len() <= DIAGNOSTIC_ITEMS_PAGE_BYTES);
        assert!(items.len() < 400);
        assert_eq!(
            items.last().unwrap()["message"],
            DIAGNOSTIC_OMISSION_MESSAGE
        );
    }
}
