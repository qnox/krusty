//! Bounded diagnostic payloads.
//!
//! Zed applies workspace diagnostics as they stream and freezes when one message is
//! multi-megabyte. A report that does not fit used to fail the whole request, which Zed logs
//! and then stops pulling, so the rest of the workspace never arrived. Pages stay small enough
//! to apply, and a client that cannot stream still receives a full-report page plus a refresh
//! for whatever did not fit.

use serde_json::{json, Value};

/// One `textDocument/publishDiagnostics` list, `textDocument/diagnostic` report, or
/// workspace-diagnostic progress page. Zed's workspace pull resets its timeout on each
/// `$/progress` page and merges the pages, so several of these beat one response it cannot render.
pub(super) const DIAGNOSTIC_PAGE_BYTES: usize = 256 * 1024;

/// Room for the workspace-report envelope (`uri`, `resultId`, `kind`) around one file's items,
/// so a single file still occupies one page.
const DIAGNOSTIC_REPORT_OVERHEAD_BYTES: usize = 4 * 1024;

/// Diagnostic items inside one file. The workspace page budget includes the report envelope.
pub(super) const DIAGNOSTIC_ITEMS_PAGE_BYTES: usize =
    DIAGNOSTIC_PAGE_BYTES - DIAGNOSTIC_REPORT_OVERHEAD_BYTES;

/// One diagnostic's `message` on the wire. The analysis store may retain a longer compiler
/// message; the editor payload does not.
pub(super) const DIAGNOSTIC_MESSAGE_WIRE_BYTES: usize = 8 * 1024;

pub(super) const DIAGNOSTIC_OMISSION_MESSAGE: &str =
    "Additional diagnostics omitted (response page limit).";

pub(super) struct PagedWorkspaceDiagnostics {
    /// `$/progress` notifications carrying earlier pages. Empty when the report fits in the
    /// final result, or when the client did not offer a partial-result token.
    pub progress: Vec<Value>,
    /// Items for the JSON-RPC result. With a partial-result token this is the last page;
    /// without one it is the first page, preferring reports the client does not already have.
    pub items: Vec<Value>,
    /// `true` when a non-streaming response left out a `full` report. The caller asks the
    /// client to pull again; reports already delivered come back as `unchanged` and sort behind
    /// the ones still missing, so the next page advances.
    pub omitted_full_reports: bool,
}

pub(super) fn wire_diagnostic_message(message: &str) -> std::borrow::Cow<'_, str> {
    if message.len() <= DIAGNOSTIC_MESSAGE_WIRE_BYTES {
        return std::borrow::Cow::Borrowed(message);
    }
    let ellipsis = "…";
    let mut end = DIAGNOSTIC_MESSAGE_WIRE_BYTES.saturating_sub(ellipsis.len());
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = String::with_capacity(end.saturating_add(ellipsis.len()));
    truncated.push_str(&message[..end]);
    truncated.push_str(ellipsis);
    std::borrow::Cow::Owned(truncated)
}

pub(super) fn limit_diagnostic_items(items: Vec<Value>) -> Vec<Value> {
    fit_json_array(items, DIAGNOSTIC_ITEMS_PAGE_BYTES, true)
}

pub(super) fn page_workspace_diagnostics(
    items: Vec<Value>,
    partial_result_token: Option<&Value>,
) -> PagedWorkspaceDiagnostics {
    if let Some(token) =
        partial_result_token.filter(|token| matches!(*token, Value::String(_) | Value::Number(_)))
    {
        let mut pages = split_pages(items);
        let last = pages.pop().unwrap_or_default();
        let progress = pages
            .into_iter()
            .map(|page| progress_notification(token, page))
            .collect();
        return PagedWorkspaceDiagnostics {
            progress,
            items: last,
            omitted_full_reports: false,
        };
    }

    let mut pages = split_pages(full_reports_first(items));
    let first = if pages.is_empty() {
        Vec::new()
    } else {
        pages.remove(0)
    };
    let omitted_full_reports = pages
        .iter()
        .flatten()
        .any(|item| item.get("kind").and_then(Value::as_str) == Some("full"));
    PagedWorkspaceDiagnostics {
        progress: Vec::new(),
        items: first,
        omitted_full_reports,
    }
}

fn full_reports_first(items: Vec<Value>) -> Vec<Value> {
    let (full, unchanged): (Vec<Value>, Vec<Value>) = items
        .into_iter()
        .partition(|item| item.get("kind").and_then(Value::as_str) == Some("full"));
    full.into_iter().chain(unchanged).collect()
}

fn split_pages(items: Vec<Value>) -> Vec<Vec<Value>> {
    let mut pages = Vec::new();
    let mut page = Vec::new();
    let mut used = 2usize;
    for item in items {
        let len = json_len(&item);
        let separator = usize::from(!page.is_empty());
        if !page.is_empty()
            && used.saturating_add(separator).saturating_add(len) > DIAGNOSTIC_PAGE_BYTES
        {
            pages.push(std::mem::take(&mut page));
            used = 2;
        }
        let separator = usize::from(!page.is_empty());
        used = used.saturating_add(separator).saturating_add(len);
        page.push(item);
    }
    if !page.is_empty() || pages.is_empty() {
        pages.push(page);
    }
    pages
}

fn progress_notification(token: &Value, items: Vec<Value>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": {
            "token": token,
            "value": { "items": items }
        }
    })
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

fn json_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|encoded| encoded.len())
        .unwrap_or(usize::MAX)
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

    #[test]
    fn a_partial_result_token_streams_every_report_without_an_error() {
        let message = "m".repeat(8 * 1024);
        let items: Vec<Value> = (0..80)
            .map(|index| full_item(&format!("file:///w/F{index:03}.kt"), &message))
            .collect();
        let token = json!("workspace/diagnostic/1");
        let paged = page_workspace_diagnostics(items.clone(), Some(&token));
        assert!(!paged.omitted_full_reports);
        assert!(!paged.progress.is_empty());

        let mut seen = Vec::new();
        for message in paged
            .progress
            .iter()
            .chain(std::iter::once(&json!({"items": paged.items})))
        {
            let items = message
                .pointer("/params/value/items")
                .or_else(|| message.get("items"))
                .and_then(Value::as_array)
                .unwrap();
            let encoded = serde_json::to_vec(items).unwrap();
            assert!(
                encoded.len() <= DIAGNOSTIC_PAGE_BYTES || items.len() == 1,
                "page is {} bytes across {} items",
                encoded.len(),
                items.len()
            );
            seen.extend(
                items
                    .iter()
                    .map(|item| item["uri"].as_str().unwrap().to_string()),
            );
        }
        let expected: Vec<String> = (0..80)
            .map(|index| format!("file:///w/F{index:03}.kt"))
            .collect();
        assert_eq!(seen, expected);
        assert_eq!(paged.progress[0]["method"], "$/progress");
        assert_eq!(paged.progress[0]["params"]["token"], token);
    }

    #[test]
    fn without_a_token_the_first_page_prefers_full_reports_still_missing() {
        let message = "m".repeat(8 * 1024);
        let mut items = vec![unchanged_item("file:///w/Already.kt")];
        items
            .extend((0..80).map(|index| full_item(&format!("file:///w/F{index:03}.kt"), &message)));
        let paged = page_workspace_diagnostics(items, None);
        assert!(paged.progress.is_empty());
        assert!(paged.omitted_full_reports);
        assert!(paged.items.iter().all(|item| item["kind"] == "full"));
        assert!(paged.items.len() < 80);
        let encoded = serde_json::to_vec(&Value::Array(paged.items)).unwrap();
        assert!(encoded.len() <= DIAGNOSTIC_PAGE_BYTES);
    }
}
