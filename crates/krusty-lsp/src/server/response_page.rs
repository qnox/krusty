//! Bounded editor responses.
//!
//! A list that fits in one [`RESPONSE_PAGE_BYTES`] frame is the JSON-RPC result. A client that
//! sends a `partialResultToken` and a list that does not fit is streamed with `$/progress`: every
//! page, including the last, is a progress notification, and the final result is empty. LSP 3.17
//! requires that split once any partial result is reported. A list that still does not fit, or a
//! request with no token whose list does not fit one frame, is a server-cancelled error. Identity,
//! location, and edit strings are never rewritten to make a value fit.

use std::borrow::Cow;
use std::collections::VecDeque;

use serde_json::{json, Map, Value};

use super::implementation::MAX_MESSAGE_BYTES;

/// One editor-facing frame, including the JSON-RPC envelope, id, and progress token.
pub(super) const RESPONSE_PAGE_BYTES: usize = 256 * 1024;

/// How many progress pages one request may emit.
pub(super) const MAX_RESPONSE_PAGES: usize = 32;

/// Hover markdown kept on the wire. The stored hover text may be longer.
pub(super) const HOVER_TEXT_BYTES: usize = 8 * 1024;

const SERVER_CANCELLED: i32 = -32802;

/// `partialResultToken` when it is a string or integer. A missing token is absence. Any other
/// JSON shape is invalid params.
pub(super) fn partial_result_token(params: &Value) -> Result<Option<Value>, ()> {
    match params.get("partialResultToken") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(Value::String(text.clone()))),
        Some(Value::Number(number)) if number.is_i64() || number.is_u64() => {
            Ok(Some(Value::Number(number.clone())))
        }
        Some(_) => Err(()),
    }
}

pub(super) fn limit_text(text: &str, max_bytes: usize) -> Cow<'_, str> {
    if text.len() <= max_bytes {
        return Cow::Borrowed(text);
    }
    let ellipsis = "…";
    let mut end = max_bytes.saturating_sub(ellipsis.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = String::with_capacity(end.saturating_add(ellipsis.len()));
    truncated.push_str(&text[..end]);
    truncated.push_str(ellipsis);
    if truncated.len() > max_bytes {
        Cow::Owned(String::new())
    } else {
        Cow::Owned(truncated)
    }
}

/// Whole completion items that fit in one result frame. Dropped items are not rewritten; the
/// caller marks the list incomplete.
pub(super) fn fit_completion_items(
    id: &Value,
    items: Vec<Value>,
) -> Result<(Vec<Value>, bool), ()> {
    let shell = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {"isIncomplete": false, "items": []}
    });
    let Some(budget) = array_budget(&shell) else {
        return Err(());
    };
    let mut kept = Vec::new();
    let mut used = 2usize;
    for item in items {
        let len = json_len(&item);
        let separator = usize::from(!kept.is_empty());
        if len.saturating_add(2) > budget
            || used.saturating_add(separator).saturating_add(len) > budget
        {
            return Ok((kept, true));
        }
        used = used.saturating_add(separator).saturating_add(len);
        kept.push(item);
    }
    Ok((kept, false))
}

/// Signature help that fits one result frame. Extra signatures are dropped whole and
/// `activeSignature` stays inside the remaining array. Labels are not rewritten.
pub(super) fn limit_signature_help(id: &Value, value: Value) -> Result<Value, ()> {
    let Some(budget) = result_value_budget(id) else {
        return Err(());
    };
    if json_len(&value) <= budget {
        return Ok(value);
    }
    let Value::Object(object) = value else {
        return Ok(Value::Null);
    };
    let mut object = object;
    let Some(Value::Array(mut signatures)) = object.remove("signatures") else {
        return Ok(Value::Null);
    };
    while !signatures.is_empty() {
        clamp_active_signature(&mut object, signatures.len());
        object.insert("signatures".to_string(), Value::Array(signatures.clone()));
        let candidate = Value::Object(object.clone());
        if json_len(&candidate) <= budget {
            return Ok(candidate);
        }
        signatures.pop();
    }
    Ok(Value::Null)
}

pub(super) fn array_messages(id: Value, items: Vec<Value>, token: Option<&Value>) -> Vec<Value> {
    paged_array_messages(
        id,
        items,
        token,
        |id, items| result_message(id, Value::Array(items)),
        |token, items| progress_message(token, Value::Array(items)),
        too_large,
    )
}

pub(super) fn location_messages(
    id: Value,
    locations: Vec<Value>,
    token: Option<&Value>,
) -> Vec<Value> {
    if locations.is_empty() {
        vec![result_message(&id, Value::Null)]
    } else {
        array_messages(id, locations, token)
    }
}

pub(super) enum ArrayPagePlan {
    One(Vec<Value>),
    Pages(VecDeque<Vec<Value>>),
    TooLarge,
}

pub(super) fn paged_array_plan<ResultFrame, ProgressFrame>(
    id: &Value,
    items: Vec<Value>,
    token: Option<&Value>,
    result_frame: &ResultFrame,
    progress_frame: &ProgressFrame,
) -> ArrayPagePlan
where
    ResultFrame: Fn(&Value, Vec<Value>) -> Value,
    ProgressFrame: Fn(&Value, Vec<Value>) -> Value,
{
    let Some(result_budget) = array_budget(&result_frame(id, Vec::new())) else {
        return ArrayPagePlan::TooLarge;
    };
    if fits_one(&items, result_budget) {
        return ArrayPagePlan::One(items);
    }
    let Some(token) = token else {
        return ArrayPagePlan::TooLarge;
    };
    let Some(progress_budget) = array_budget(&progress_frame(token, Vec::new())) else {
        return ArrayPagePlan::TooLarge;
    };
    match pack_values(items, progress_budget, MAX_RESPONSE_PAGES) {
        Ok(pages) => ArrayPagePlan::Pages(pages.into()),
        Err(()) => ArrayPagePlan::TooLarge,
    }
}

/// Build one bounded array result or an all-or-nothing partial-result stream. The caller owns only
/// the protocol-specific shape around the array; token validation, envelope budgets, page packing,
/// the page cap, and failure shape stay common to every editor response.
pub(super) fn paged_array_messages<ResultFrame, ProgressFrame, TooLarge>(
    id: Value,
    items: Vec<Value>,
    token: Option<&Value>,
    result_frame: ResultFrame,
    progress_frame: ProgressFrame,
    too_large: TooLarge,
) -> Vec<Value>
where
    ResultFrame: Fn(&Value, Vec<Value>) -> Value,
    ProgressFrame: Fn(&Value, Vec<Value>) -> Value,
    TooLarge: Fn(&Value) -> Value,
{
    match paged_array_plan(&id, items, token, &result_frame, &progress_frame) {
        ArrayPagePlan::One(items) => vec![result_frame(&id, items)],
        ArrayPagePlan::Pages(pages) => {
            let token = token.expect("a page plan requires a partial-result token");
            let mut messages = Vec::with_capacity(pages.len().saturating_add(1));
            messages.extend(pages.into_iter().map(|page| progress_frame(token, page)));
            messages.push(result_frame(&id, Vec::new()));
            messages
        }
        ArrayPagePlan::TooLarge => vec![too_large(&id)],
    }
}

fn clamp_active_signature(object: &mut Map<String, Value>, len: usize) {
    let Some(active) = object.get("activeSignature").and_then(Value::as_u64) else {
        return;
    };
    if active >= len as u64 {
        object.insert(
            "activeSignature".to_string(),
            Value::Number((len - 1).into()),
        );
    }
}

fn result_value_budget(id: &Value) -> Option<usize> {
    let shell = result_message(id, Value::Null);
    let frame = json_len(&shell);
    if frame > RESPONSE_PAGE_BYTES {
        return None;
    }
    // The shell measures a four-byte `null` result. The value replaces it.
    Some(RESPONSE_PAGE_BYTES - frame + 4)
}

fn array_budget(empty_frame: &Value) -> Option<usize> {
    let frame = json_len(empty_frame);
    if frame > RESPONSE_PAGE_BYTES {
        return None;
    }
    // `empty_frame` already contains `[]`. `used` starts at those two bytes.
    Some(RESPONSE_PAGE_BYTES - frame + 2)
}

fn fits_one(items: &[Value], budget: usize) -> bool {
    if budget < 2 {
        return items.is_empty();
    }
    let mut used = 2usize;
    for item in items {
        let len = json_len(item);
        if len.saturating_add(2) > budget {
            return false;
        }
        let separator = usize::from(used > 2);
        if used.saturating_add(separator).saturating_add(len) > budget {
            return false;
        }
        used = used.saturating_add(separator).saturating_add(len);
    }
    true
}

fn pack_values(items: Vec<Value>, budget: usize, max_pages: usize) -> Result<Vec<Vec<Value>>, ()> {
    if max_pages == 0 || budget < 2 {
        return Err(());
    }
    let mut pages = Vec::new();
    let mut page = Vec::new();
    let mut used = 2usize;
    for item in items {
        let len = json_len(&item);
        if len.saturating_add(2) > budget {
            return Err(());
        }
        let separator = usize::from(!page.is_empty());
        if used.saturating_add(separator).saturating_add(len) > budget {
            pages.push(std::mem::take(&mut page));
            if pages.len() == max_pages {
                return Err(());
            }
            used = 2;
        }
        let separator = usize::from(!page.is_empty());
        used = used.saturating_add(separator).saturating_add(len);
        page.push(item);
    }
    if page.is_empty() && pages.is_empty() {
        pages.push(page);
    } else if !page.is_empty() {
        if pages.len() == max_pages {
            return Err(());
        }
        pages.push(page);
    }
    Ok(pages)
}

fn result_message(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn progress_message(token: &Value, value: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": {"token": token, "value": value}
    })
}

pub(super) fn too_large(id: &Value) -> Value {
    server_cancelled(id, "response exceeds the editor page limit", None)
}

/// A bounded server-cancelled response. A request id that cannot fit is replaced by `null`, so an
/// error path never violates the stdout queue's frame invariant while reporting that violation.
pub(super) fn server_cancelled(id: &Value, message: &str, data: Option<Value>) -> Value {
    let response = error_message(id, SERVER_CANCELLED, message, data.as_ref());
    if json_len(&response) <= MAX_MESSAGE_BYTES {
        response
    } else {
        error_message(&Value::Null, SERVER_CANCELLED, message, data.as_ref())
    }
}

fn error_message(id: &Value, code: i32, message: &str, data: Option<&Value>) -> Value {
    let mut error = json!({
        "code": code,
        "message": message
    });
    if let (Value::Object(error), Some(data)) = (&mut error, data) {
        error.insert("data".to_string(), data.clone());
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error
    })
}

pub(super) fn json_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|encoded| encoded.len())
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_and_signature_help_fail_when_the_empty_envelope_does_not_fit() {
        use super::super::implementation::LspService;
        use crate::DocumentAnalysis;

        let mut service = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| DocumentAnalysis::empty())
                .collect::<Vec<_>>()
        });
        service.force_initialized_for_test();
        let uri = "file:///w/Main.kt";
        service.open_document_for_test(uri, "fun main() = Unit\n", 1);
        let id = Value::String("i".repeat(RESPONSE_PAGE_BYTES));

        for method in ["textDocument/completion", "textDocument/signatureHelp"] {
            let dispatch = service.handle(json!({
                "jsonrpc": "2.0",
                "id": id.clone(),
                "method": method,
                "params": {
                    "textDocument": {"uri": uri},
                    "position": {"line": 0, "character": 0}
                }
            }));
            assert_eq!(dispatch.messages.len(), 1, "{method}");
            assert_eq!(dispatch.messages[0]["error"]["code"], -32802, "{method}");
            assert!(dispatch.messages[0].get("result").is_none(), "{method}");
        }
    }

    fn symbol(name: &str) -> Value {
        json!({
            "name": name,
            "kind": 5,
            "range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 1}
            },
            "selectionRange": {
                "start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 1}
            }
        })
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
    fn a_long_hover_is_cut_on_a_char_boundary() {
        let text = "é".repeat(HOVER_TEXT_BYTES);
        let limited = limit_text(&text, HOVER_TEXT_BYTES);
        assert!(limited.len() <= HOVER_TEXT_BYTES);
        assert!(limited.ends_with('…'));
        assert!(limited.starts_with('é'));
        assert_eq!(limit_text("short", HOVER_TEXT_BYTES).as_ref(), "short");
    }

    #[test]
    fn partial_result_token_accepts_string_and_integer_only() {
        assert_eq!(partial_result_token(&json!({})).unwrap(), None);
        assert_eq!(
            partial_result_token(&json!({"partialResultToken": null})).unwrap(),
            None
        );
        assert_eq!(
            partial_result_token(&json!({"partialResultToken": "sem/1"})).unwrap(),
            Some(json!("sem/1"))
        );
        assert_eq!(
            partial_result_token(&json!({"partialResultToken": 7})).unwrap(),
            Some(json!(7))
        );
        assert!(partial_result_token(&json!({"partialResultToken": true})).is_err());
        assert!(partial_result_token(&json!({"partialResultToken": {"t": 1}})).is_err());
        assert!(partial_result_token(&json!({"partialResultToken": 1.5})).is_err());
        assert!(partial_result_token(&json!({"partialResultToken": []})).is_err());
    }

    #[test]
    fn a_token_streams_every_page_and_the_final_result_is_empty() {
        let item = json!({"message": "m".repeat(8 * 1024)});
        let items = vec![item; 80];
        let id = json!("list-1");
        let token = json!(7);
        let messages = array_messages(id.clone(), items.clone(), Some(&token));
        assert!(messages.len() > 2);
        assert_frames_fit(&messages);
        assert_eq!(messages.last().unwrap()["id"], id);
        assert_eq!(messages.last().unwrap()["result"], json!([]));
        let mut seen = Vec::new();
        for message in &messages[..messages.len() - 1] {
            assert_eq!(message["method"], "$/progress");
            assert_eq!(message["params"]["token"], token);
            let page = message["params"]["value"].as_array().unwrap();
            assert!(!page.is_empty());
            seen.extend(page.iter().cloned());
        }
        assert_eq!(seen, items);
    }

    #[test]
    fn a_list_that_fits_one_frame_stays_in_the_result_even_with_a_token() {
        let items = vec![json!({"name": "Answer"})];
        let messages = array_messages(json!(1), items.clone(), Some(&json!("tok")));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["result"], Value::Array(items));
        assert!(messages[0].get("method").is_none());
    }

    #[test]
    fn a_list_without_a_token_that_does_not_fit_is_an_error() {
        let item = json!({"message": "m".repeat(8 * 1024)});
        let messages = array_messages(json!("id"), vec![item; 80], None);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["error"]["code"], SERVER_CANCELLED);
        assert!(messages[0].get("result").is_none());
    }

    #[test]
    fn streaming_past_the_page_cap_is_an_error_without_a_prefix() {
        let item = json!({"message": "m".repeat(8 * 1024)});
        let messages = array_messages(json!("cap"), vec![item; 1_500], Some(&json!("cap")));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["error"]["code"], SERVER_CANCELLED);
        assert!(messages[0].get("method").is_none());
    }

    #[test]
    fn an_item_that_does_not_fit_one_page_is_not_rewritten() {
        let root = json!({
            "name": "Root",
            "children": (0..2_000).map(|index| symbol(&format!("member{index:04}"))).collect::<Vec<_>>()
        });
        let messages = array_messages(json!(1), vec![root], None);
        assert_eq!(messages[0]["error"]["code"], SERVER_CANCELLED);
        let encoded = serde_json::to_vec(&messages[0]).unwrap();
        assert!(!encoded.windows(6).any(|window| window == b"member"));
    }

    #[test]
    fn a_large_request_id_is_charged_in_the_frame() {
        let id = Value::String("i".repeat(RESPONSE_PAGE_BYTES));
        let messages = array_messages(id, vec![json!(1)], None);
        assert_eq!(messages[0]["error"]["code"], SERVER_CANCELLED);
        assert!(serde_json::to_vec(&messages[0]).unwrap().len() <= MAX_MESSAGE_BYTES);
    }

    #[test]
    fn a_large_token_cannot_stream_a_list_that_needs_another_page() {
        let token = Value::String("t".repeat(RESPONSE_PAGE_BYTES));
        let item = json!({"message": "m".repeat(8 * 1024)});
        let messages = array_messages(json!(1), vec![item; 80], Some(&token));
        assert_eq!(messages[0]["error"]["code"], SERVER_CANCELLED);
        let encoded = serde_json::to_vec(&messages[0]).unwrap();
        assert!(!encoded.windows(8).any(|window| window == b"tttttttt"));
    }

    #[test]
    fn completion_items_are_dropped_whole_and_labels_stay_intact() {
        let items: Vec<Value> = (0..400)
            .map(|index| json!({"label": format!("item{index:04}{}", "x".repeat(2 * 1024))}))
            .collect();
        let (kept, truncated) = fit_completion_items(&json!(1), items.clone()).unwrap();
        assert!(truncated);
        assert!(!kept.is_empty());
        assert!(kept.len() < items.len());
        assert_eq!(kept[0]["label"], items[0]["label"]);
        let shell = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"isIncomplete": false, "items": kept}
        });
        assert!(json_len(&shell) <= RESPONSE_PAGE_BYTES);
    }

    #[test]
    fn signature_help_drops_signatures_and_keeps_the_active_index_valid() {
        let signatures: Vec<Value> = (0..800)
            .map(
                |index| json!({"label": format!("fun overloaded{index}({})", "Int, ".repeat(200))}),
            )
            .collect();
        let value = json!({"signatures": signatures, "activeSignature": 799});
        let limited = limit_signature_help(&json!("sig"), value).unwrap();
        let kept = limited["signatures"].as_array().unwrap();
        assert!(!kept.is_empty());
        assert!(kept.len() < 800);
        let active = limited["activeSignature"].as_u64().unwrap();
        assert!(active < kept.len() as u64);
        assert_eq!(
            kept[0]["label"],
            json!(format!("fun overloaded0({})", "Int, ".repeat(200)))
        );
        let shell = result_message(&json!("sig"), limited);
        assert!(json_len(&shell) <= RESPONSE_PAGE_BYTES);
    }

    #[test]
    fn one_signature_that_does_not_fit_is_absent() {
        let value = json!({
            "signatures": [{"label": "L".repeat(RESPONSE_PAGE_BYTES)}],
            "activeSignature": 0
        });
        assert_eq!(limit_signature_help(&json!(1), value), Ok(Value::Null));
    }
}
