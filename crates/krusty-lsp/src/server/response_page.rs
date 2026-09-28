//! Bounded editor responses.
//!
//! Zed parses each JSON-RPC frame on the UI path. A multi-megabyte array freezes that parse, and
//! building the array can exhaust the process. List results are cut into pages of
//! [`RESPONSE_PAGE_BYTES`]. A client that sends `partialResultToken` receives every page that fits
//! in [`MAX_RESPONSE_PAGES`] as `$/progress`, then the last page as the result. A client that does
//! not (Zed, for these methods) receives the first page as a normal result.

use std::borrow::Cow;

use serde_json::{json, Value};

use crate::analysis::semantic_token_json_cost;

/// One editor-facing list page, including the JSON-RPC envelope.
pub(super) const RESPONSE_PAGE_BYTES: usize = 256 * 1024;

const RESPONSE_ENVELOPE_BYTES: usize = 4 * 1024;

/// JSON array (or semantic-token `data` array) inside one page.
pub(super) const RESPONSE_ITEMS_PAGE_BYTES: usize = RESPONSE_PAGE_BYTES - RESPONSE_ENVELOPE_BYTES;

/// How many pages one request may emit. Past this the rest is omitted so a streamed response
/// cannot grow without a bound.
pub(super) const MAX_RESPONSE_PAGES: usize = 32;

/// Hover markdown kept on the wire. The stored hover text may be longer.
pub(super) const HOVER_TEXT_BYTES: usize = 8 * 1024;

pub(super) struct PagedJsonArray {
    /// `$/progress` notifications for every page except the last.
    pub progress: Vec<Value>,
    /// Items for the JSON-RPC result.
    pub items: Vec<Value>,
}

pub(super) struct PagedSemanticTokens {
    pub progress: Vec<Value>,
    pub data: Vec<u32>,
}

pub(super) fn response_item_budget(streaming: bool) -> usize {
    if streaming {
        RESPONSE_ITEMS_PAGE_BYTES.saturating_mul(MAX_RESPONSE_PAGES)
    } else {
        RESPONSE_ITEMS_PAGE_BYTES
    }
}

/// `partialResultToken` when it is a string or number. Other JSON is ignored.
pub(super) fn partial_result_token(params: &Value) -> Option<Value> {
    params.get("partialResultToken").and_then(|token| {
        matches!(token, Value::String(_) | Value::Number(_)).then(|| token.clone())
    })
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

pub(super) fn page_json_array(
    items: Vec<Value>,
    partial_result_token: Option<&Value>,
) -> PagedJsonArray {
    let Some(token) =
        partial_result_token.filter(|token| matches!(*token, Value::String(_) | Value::Number(_)))
    else {
        return PagedJsonArray {
            progress: Vec::new(),
            items: fit_json_array(items, RESPONSE_ITEMS_PAGE_BYTES),
        };
    };
    let mut pages = split_value_pages(items, RESPONSE_ITEMS_PAGE_BYTES, MAX_RESPONSE_PAGES);
    let last = pages.pop().unwrap_or_default();
    let progress = pages
        .into_iter()
        .map(|page| progress_notification(token, Value::Array(page)))
        .collect();
    PagedJsonArray {
        progress,
        items: last,
    }
}

pub(super) fn page_semantic_token_data(
    data: Vec<u32>,
    partial_result_token: Option<&Value>,
) -> PagedSemanticTokens {
    let streaming =
        partial_result_token.filter(|token| matches!(*token, Value::String(_) | Value::Number(_)));
    let max_pages = if streaming.is_some() {
        MAX_RESPONSE_PAGES
    } else {
        1
    };
    let mut pages = split_token_pages(&data, RESPONSE_ITEMS_PAGE_BYTES, max_pages);
    let last = pages.pop().unwrap_or_default();
    let progress = match streaming {
        Some(token) => pages
            .into_iter()
            .map(|page| progress_notification(token, json!({ "data": page })))
            .collect(),
        None => Vec::new(),
    };
    PagedSemanticTokens {
        progress,
        data: last,
    }
}

/// Running JSON-array size, so a collector can stop without encoding past the budget.
pub(super) struct ArrayBudget {
    used: usize,
    limit: usize,
}

impl ArrayBudget {
    pub(super) fn new(limit: usize) -> Self {
        Self { used: 2, limit }
    }

    pub(super) fn admit(&mut self, item: &Value) -> bool {
        let len = json_len(item);
        let separator = usize::from(self.used > 2);
        if self.used.saturating_add(separator).saturating_add(len) > self.limit {
            return false;
        }
        self.used = self.used.saturating_add(separator).saturating_add(len);
        true
    }
}

pub(super) fn limit_signature_help(mut value: Value) -> Value {
    if json_len(&value) <= RESPONSE_ITEMS_PAGE_BYTES {
        return value;
    }
    shrink_value(&mut value, RESPONSE_ITEMS_PAGE_BYTES);
    if json_len(&value) <= RESPONSE_ITEMS_PAGE_BYTES {
        value
    } else {
        Value::Null
    }
}

fn progress_notification(token: &Value, value: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": {
            "token": token,
            "value": value
        }
    })
}

fn fit_json_array(items: Vec<Value>, budget: usize) -> Vec<Value> {
    if budget < 2 {
        return Vec::new();
    }
    let mut kept = Vec::new();
    let mut used = 2usize;
    for mut item in items {
        let mut len = json_len(&item);
        if len.saturating_add(2) > budget {
            shrink_value(&mut item, budget.saturating_sub(2));
            len = json_len(&item);
        }
        let separator = usize::from(!kept.is_empty());
        if used.saturating_add(separator).saturating_add(len) > budget {
            break;
        }
        used = used.saturating_add(separator).saturating_add(len);
        kept.push(item);
    }
    kept
}

fn split_value_pages(items: Vec<Value>, budget: usize, max_pages: usize) -> Vec<Vec<Value>> {
    let mut pages = Vec::new();
    let mut page = Vec::new();
    let mut used = 2usize;
    if max_pages == 0 || budget < 2 {
        return vec![Vec::new()];
    }
    for mut item in items {
        let mut len = json_len(&item);
        if len.saturating_add(2) > budget {
            shrink_value(&mut item, budget.saturating_sub(2));
            len = json_len(&item);
        }
        let separator = usize::from(!page.is_empty());
        if !page.is_empty() && used.saturating_add(separator).saturating_add(len) > budget {
            if pages.len() + 1 >= max_pages {
                break;
            }
            pages.push(std::mem::take(&mut page));
            used = 2;
        }
        let separator = usize::from(!page.is_empty());
        if used.saturating_add(separator).saturating_add(len) > budget {
            break;
        }
        used = used.saturating_add(separator).saturating_add(len);
        page.push(item);
    }
    if !page.is_empty() || pages.is_empty() {
        pages.push(page);
    }
    pages
}

fn split_token_pages(data: &[u32], budget: usize, max_pages: usize) -> Vec<Vec<u32>> {
    let mut pages = Vec::new();
    let mut page = Vec::new();
    let mut used = 2usize;
    if max_pages == 0 || budget < 2 {
        return vec![Vec::new()];
    }
    for chunk in data.as_chunks::<5>().0 {
        let mut cost = semantic_token_json_cost(chunk, page.is_empty());
        if !page.is_empty() && used.saturating_add(cost) > budget {
            if pages.len() + 1 >= max_pages {
                break;
            }
            pages.push(std::mem::take(&mut page));
            used = 2;
            cost = semantic_token_json_cost(chunk, true);
        }
        if used.saturating_add(cost) > budget {
            break;
        }
        used = used.saturating_add(cost);
        page.extend_from_slice(chunk);
    }
    if !page.is_empty() || pages.is_empty() {
        pages.push(page);
    }
    pages
}

fn shrink_value(value: &mut Value, budget: usize) {
    if json_len(value) <= budget {
        return;
    }
    match value {
        Value::Object(_) => {
            shrink_object_array_field(value, "children", budget);
            if json_len(value) > budget {
                shrink_object_array_field(value, "signatures", budget);
            }
            if json_len(value) > budget {
                shrink_object_array_field(value, "items", budget);
            }
            if json_len(value) > budget {
                truncate_object_strings(value, budget);
            }
        }
        Value::Array(items) => {
            let fitted = fit_json_array(std::mem::take(items), budget);
            *value = Value::Array(fitted);
        }
        Value::String(text) => {
            *text = limit_text(text, budget.saturating_sub(2)).into_owned();
        }
        _ => {}
    }
}

fn shrink_object_array_field(value: &mut Value, field: &str, budget: usize) {
    let Some(array_len) = value.get(field).map(json_len) else {
        return;
    };
    let full = json_len(value);
    if full <= budget {
        return;
    }
    let array_budget = budget.saturating_sub(full.saturating_sub(array_len));
    let Some(items) = value
        .as_object_mut()
        .and_then(|object| object.get_mut(field))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    let fitted = fit_json_array(std::mem::take(items), array_budget);
    *items = fitted;
}

fn truncate_object_strings(value: &mut Value, budget: usize) {
    let keys: Vec<String> = value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    for key in keys {
        if json_len(value) <= budget {
            return;
        }
        let Some(limited) = value
            .get(&key)
            .and_then(Value::as_str)
            .map(|text| limit_text(text, 64).into_owned())
        else {
            continue;
        };
        if let Some(slot) = value
            .as_object_mut()
            .and_then(|object| object.get_mut(&key))
        {
            *slot = Value::String(limited);
        }
    }
}

fn json_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|encoded| encoded.len())
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::SemanticTokenIndex;

    fn symbol(name: &str, children: Vec<Value>) -> Value {
        let mut value = json!({
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
        });
        if !children.is_empty() {
            value
                .as_object_mut()
                .unwrap()
                .insert("children".to_string(), Value::Array(children));
        }
        value
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
    fn semantic_token_pages_cover_every_token_and_each_page_fits() {
        let mut data = Vec::new();
        for _ in 0..80_000 {
            data.extend_from_slice(&[1, 0, 1, 0, 0]);
        }
        let token = json!("sem/1");
        let paged = page_semantic_token_data(data.clone(), Some(&token));
        assert!(!paged.progress.is_empty());

        let mut combined = Vec::new();
        for message in paged
            .progress
            .iter()
            .chain(std::iter::once(&json!({ "data": paged.data.clone() })))
        {
            let page = message
                .pointer("/params/value/data")
                .or_else(|| message.get("data"))
                .and_then(Value::as_array)
                .unwrap();
            let encoded = serde_json::to_vec(page).unwrap();
            assert!(encoded.len() <= RESPONSE_ITEMS_PAGE_BYTES);
            assert_eq!(page.len() % 5, 0);
            combined.extend(page.iter().map(|number| number.as_u64().unwrap() as u32));
        }
        assert_eq!(combined, data);

        let first = page_semantic_token_data(data.clone(), None);
        assert!(first.progress.is_empty());
        assert!(first.data.len() < data.len());
        assert_eq!(&data[..first.data.len()], first.data.as_slice());
        assert!(serde_json::to_vec(&first.data).unwrap().len() <= RESPONSE_ITEMS_PAGE_BYTES);
    }

    #[test]
    fn encode_capped_stops_before_the_json_budget() {
        let entries: Vec<[u32; 4]> = (0..80_000).map(|line| [line, 0, 1, 0]).collect();
        let index = SemanticTokenIndex::from_entries_for_test(entries);
        let capped = index.encode_capped(None, RESPONSE_ITEMS_PAGE_BYTES);
        let full = index.encode(None);
        assert!(capped.len() < full.len());
        assert_eq!(capped.len() % 5, 0);
        assert!(serde_json::to_vec(&capped).unwrap().len() <= RESPONSE_ITEMS_PAGE_BYTES);
        assert_eq!(&full[..capped.len()], capped.as_slice());
    }

    #[test]
    fn a_partial_result_token_streams_list_pages_and_a_missing_token_keeps_the_first() {
        let item = json!({"message": "m".repeat(8 * 1024)});
        let items = vec![item; 80];
        let token = json!(7);
        let paged = page_json_array(items.clone(), Some(&token));
        assert!(!paged.progress.is_empty());
        let mut seen = 0usize;
        for message in paged
            .progress
            .iter()
            .chain(std::iter::once(&json!(paged.items.clone())))
        {
            let page = message
                .pointer("/params/value")
                .or_else(|| message.as_array().map(|_| message))
                .and_then(Value::as_array)
                .unwrap();
            assert!(serde_json::to_vec(page).unwrap().len() <= RESPONSE_ITEMS_PAGE_BYTES);
            seen += page.len();
        }
        assert_eq!(seen, items.len());

        let first = page_json_array(items, None);
        assert!(first.progress.is_empty());
        assert!(first.items.len() < 80);
        assert!(serde_json::to_vec(&first.items).unwrap().len() <= RESPONSE_ITEMS_PAGE_BYTES);
    }

    #[test]
    fn streaming_stops_after_the_page_cap() {
        let item = json!({"message": "m".repeat(8 * 1024)});
        let items = vec![item; 1_500];
        let paged = page_json_array(items, Some(&json!("cap")));
        let page_count = paged.progress.len() + usize::from(!paged.items.is_empty());
        assert!(page_count <= MAX_RESPONSE_PAGES);
        assert!(paged.progress.len() >= 2);
        let kept: usize = paged
            .progress
            .iter()
            .map(|message| {
                message
                    .pointer("/params/value")
                    .and_then(Value::as_array)
                    .unwrap()
                    .len()
            })
            .sum::<usize>()
            + paged.items.len();
        assert!(kept < 1_500);
    }

    #[test]
    fn a_symbol_whose_children_exceed_the_page_keeps_a_fitting_prefix() {
        let child = symbol(&"member".repeat(40), Vec::new());
        let root = symbol("Root", vec![child; 2_000]);
        let paged = page_json_array(vec![root], None);
        assert_eq!(paged.items.len(), 1);
        let encoded = serde_json::to_vec(&paged.items).unwrap();
        assert!(encoded.len() <= RESPONSE_ITEMS_PAGE_BYTES);
        let children = paged.items[0]["children"].as_array().unwrap();
        assert!(!children.is_empty());
        assert!(children.len() < 2_000);
    }

    #[test]
    fn signature_help_shrinks_to_the_page() {
        let signatures: Vec<Value> = (0..800)
            .map(
                |index| json!({"label": format!("fun overloaded{index}({})", "Int, ".repeat(200))}),
            )
            .collect();
        let value = json!({"signatures": signatures, "activeSignature": 0});
        assert!(json_len(&value) > RESPONSE_ITEMS_PAGE_BYTES);
        let limited = limit_signature_help(value);
        assert!(json_len(&limited) <= RESPONSE_ITEMS_PAGE_BYTES);
        assert!(limited["signatures"].as_array().unwrap().len() < 800);
    }
}
