//! Semantic-token frames.
//!
//! Full and range requests delta-encode directly into editor pages. A result that fits one frame
//! is the JSON-RPC result. A longer result with a partial-result token is sent entirely through
//! `$/progress`, and the final `data` array is empty. A result that does not fit is server
//! cancelled rather than a prefix of the token stream.

use serde::Deserialize;
use serde_json::{json, Value};

use super::implementation::{Dispatch, LspService, OpenDocument};
use super::response_page::{partial_result_token, MAX_RESPONSE_PAGES, RESPONSE_PAGE_BYTES};
use crate::analysis::SemanticTokenRange;

const SERVER_CANCELLED: i32 = -32802;

#[derive(Deserialize)]
struct TextDocumentIdentifier {
    uri: String,
}

#[derive(Deserialize)]
struct Position {
    line: u32,
    character: u32,
}

#[derive(Deserialize)]
struct Range {
    start: Position,
    end: Position,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FullParams {
    text_document: TextDocumentIdentifier,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RangeParams {
    text_document: TextDocumentIdentifier,
    range: Range,
}

struct Prepared {
    uri: String,
    range: Option<SemanticTokenRange>,
    token: Option<Value>,
}

pub(super) fn dispatch<B>(
    service: &LspService<B>,
    id: Option<Value>,
    params: Value,
    range_request: bool,
) -> Dispatch {
    let Some(id) = id else {
        return Dispatch::none();
    };
    let Ok(prepared) = prepare(params, range_request) else {
        return invalid_params(id);
    };
    let Some(open) = service.documents.get(&prepared.uri) else {
        return Dispatch::messages(vec![result_message(id, Value::Null)]);
    };
    Dispatch::messages(messages(
        id,
        token_entries(open),
        prepared.range,
        prepared.token.as_ref(),
    ))
}

fn token_entries(open: &OpenDocument) -> &[[u32; 4]] {
    &open.semantic_tokens.entries
}

pub(super) fn messages(
    id: Value,
    entries: &[[u32; 4]],
    range: Option<SemanticTokenRange>,
    token: Option<&Value>,
) -> Vec<Value> {
    let entries = select_entries(entries, range);
    let Some(result_budget) = data_budget(&result_message(id.clone(), json!({"data": []}))) else {
        return vec![too_large(&id)];
    };
    if let Ok(pages) = encode_pages(entries, result_budget, 1) {
        let data = pages.into_iter().next().unwrap_or_default();
        return vec![result_message(id.clone(), json!({"data": data}))];
    }
    let Some(token) = token else {
        return vec![too_large(&id)];
    };
    let Some(progress_budget) = data_budget(&progress_message(token, json!({"data": []}))) else {
        return vec![too_large(&id)];
    };
    match encode_pages(entries, progress_budget, MAX_RESPONSE_PAGES) {
        Ok(pages) => {
            let mut messages = Vec::with_capacity(pages.len().saturating_add(1));
            for page in pages {
                messages.push(progress_message(token, json!({"data": page})));
            }
            messages.push(result_message(id.clone(), json!({"data": []})));
            messages
        }
        Err(()) => vec![too_large(&id)],
    }
}

fn prepare(params: Value, range_request: bool) -> Result<Prepared, ()> {
    let token = partial_result_token(&params)?;
    if range_request {
        let parsed: RangeParams = serde_json::from_value(params).map_err(|_| ())?;
        Ok(Prepared {
            uri: parsed.text_document.uri,
            range: Some(SemanticTokenRange {
                start_line: parsed.range.start.line,
                start_character: parsed.range.start.character,
                end_line: parsed.range.end.line,
                end_character: parsed.range.end.character,
            }),
            token,
        })
    } else {
        let parsed: FullParams = serde_json::from_value(params).map_err(|_| ())?;
        Ok(Prepared {
            uri: parsed.text_document.uri,
            range: None,
            token,
        })
    }
}

fn select_entries(entries: &[[u32; 4]], range: Option<SemanticTokenRange>) -> &[[u32; 4]] {
    let Some(range) = range else {
        return entries;
    };
    let start = (range.start_line, range.start_character);
    let end = (range.end_line, range.end_character);
    let first =
        entries.partition_point(|entry| (entry[0], entry[1].saturating_add(entry[2])) <= start);
    let count = entries[first..].partition_point(|entry| (entry[0], entry[1]) < end);
    &entries[first..first + count]
}

fn encode_pages(
    entries: &[[u32; 4]],
    budget: usize,
    max_pages: usize,
) -> Result<Vec<Vec<u32>>, ()> {
    if max_pages == 0 || budget < 2 {
        return Err(());
    }
    let mut pages = Vec::new();
    let mut page = Vec::new();
    let mut used = 2usize;
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
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
        let token = [
            delta_line,
            delta_start,
            entry[2],
            packed & u8::MAX as u32,
            packed >> 8,
        ];
        let cost = semantic_token_json_cost(&token, page.is_empty());
        if used.saturating_add(cost) > budget {
            if page.is_empty() || pages.len() + 1 == max_pages {
                return Err(());
            }
            pages.push(std::mem::take(&mut page));
            used = 2;
            let cost = semantic_token_json_cost(&token, true);
            if used.saturating_add(cost) > budget {
                return Err(());
            }
            used = used.saturating_add(cost);
            page.extend_from_slice(&token);
        } else {
            used = used.saturating_add(cost);
            page.extend_from_slice(&token);
        }
        previous_line = line;
        previous_start = start;
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

fn semantic_token_json_cost(token: &[u32], page_empty: bool) -> usize {
    let digits = token
        .iter()
        .map(|number| json_u32_len(*number))
        .sum::<usize>();
    let commas = token.len().saturating_sub(1);
    usize::from(!page_empty)
        .saturating_add(digits)
        .saturating_add(commas)
}

fn json_u32_len(number: u32) -> usize {
    if number == 0 {
        1
    } else {
        number.ilog10() as usize + 1
    }
}

fn data_budget(empty_frame: &Value) -> Option<usize> {
    let frame = json_len(empty_frame);
    if frame > RESPONSE_PAGE_BYTES {
        return None;
    }
    Some(RESPONSE_PAGE_BYTES - frame + 2)
}

fn result_message(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn progress_message(token: &Value, value: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": {"token": token, "value": value}
    })
}

fn too_large(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": SERVER_CANCELLED,
            "message": "response exceeds the editor page limit"
        }
    })
}

fn invalid_params(id: Value) -> Dispatch {
    Dispatch::messages(vec![json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32602, "message": "invalid params"}
    })])
}

fn json_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|encoded| encoded.len())
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::super::implementation::{AnalysisBackend, LspService, ProjectFeedback};
    use super::super::AnalysisJob;
    use crate::analysis::SemanticTokenIndex;
    use crate::server::engine::AnalysisBatch;

    #[test]
    fn streamed_tokens_cover_the_index_and_each_frame_fits() {
        let entries: Vec<[u32; 4]> = (0..80_000).map(|line| [line, 0, 1, 0]).collect();
        let index = SemanticTokenIndex {
            entries: entries.clone(),
        };
        let expected = index.encode(None);
        let id = json!("tokens");
        let token = json!("sem/1");
        let paged = messages(id.clone(), &index.entries, None, Some(&token));
        assert!(paged.len() > 2);
        assert_eq!(paged.last().unwrap()["result"]["data"], json!([]));
        let mut combined = Vec::new();
        for message in &paged[..paged.len() - 1] {
            let encoded = serde_json::to_vec(message).unwrap();
            assert!(encoded.len() <= RESPONSE_PAGE_BYTES);
            assert_eq!(message["params"]["token"], token);
            let page = message["params"]["value"]["data"].as_array().unwrap();
            assert_eq!(page.len() % 5, 0);
            assert!(!page.is_empty());
            combined.extend(page.iter().map(|number| number.as_u64().unwrap() as u32));
        }
        assert_eq!(combined, expected);

        let refused = messages(json!("tokens"), &index.entries, None, None);
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0]["error"]["code"], SERVER_CANCELLED);
        assert!(refused[0].get("result").is_none());
    }

    #[test]
    fn semantic_token_responses_page_on_the_session() {
        let mut service = LspService::with_backend(IdleBackend);
        service.force_initialized_for_test();
        let uri = "file:///wide.kt";
        service.open_document_for_test(uri, "fun box() = 1\n", 1);
        let entries: Vec<[u32; 4]> = (0..80_000).map(|line| [line, 0, 1, 0]).collect();
        service
            .documents
            .get_mut(uri)
            .unwrap()
            .semantic_tokens
            .entries = entries;
        let full = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "tokens",
            "method": "textDocument/semanticTokens/full",
            "params": {"textDocument": {"uri": uri}}
        }));
        assert_eq!(full.messages.len(), 1);
        assert_eq!(full.messages[0]["error"]["code"], SERVER_CANCELLED);

        let streamed = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "tokens",
            "method": "textDocument/semanticTokens/full",
            "params": {
                "textDocument": {"uri": uri},
                "partialResultToken": "sem/1"
            }
        }));
        assert!(streamed.messages.len() > 1);
        let last = streamed.messages.last().unwrap();
        assert_eq!(last["id"], "tokens");
        assert_eq!(last["result"]["data"], json!([]));
        let mut combined = Vec::new();
        for message in &streamed.messages[..streamed.messages.len() - 1] {
            assert!(serde_json::to_vec(message).unwrap().len() <= RESPONSE_PAGE_BYTES);
            let page = message["params"]["value"]["data"].as_array().unwrap();
            combined.extend(page.iter().map(|number| number.as_u64().unwrap() as u32));
        }
        assert_eq!(
            combined,
            service.documents[uri].semantic_tokens.encode(None)
        );

        let invalid = service.handle(json!({
            "jsonrpc": "2.0",
            "id": "tokens",
            "method": "textDocument/semanticTokens/full",
            "params": {
                "textDocument": {"uri": uri},
                "partialResultToken": true
            }
        }));
        assert_eq!(invalid.messages[0]["error"]["code"], -32602);
    }

    struct IdleBackend;

    impl AnalysisBackend for IdleBackend {
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
            false
        }
        fn project_refresh_due_in(&self) -> Option<Duration> {
            None
        }
        fn refresh_project(&mut self) -> Option<ProjectFeedback> {
            None
        }
    }
}
