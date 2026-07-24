mod implementation;
pub use implementation::*;

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Cursor;
    use std::rc::Rc;

    use serde_json::{json, Value};

    use super::*;
    use krusty::diag::{Diagnostic, Severity, Span};

    fn request(id: i64, method: &str, params: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
    }

    fn notification(method: &str, params: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        })
    }

    #[test]
    fn byte_offsets_are_reported_as_utf16_positions() {
        let text = "a😀\r\nβz";
        assert_eq!(byte_offset_to_position(text, 0), Position::new(0, 0));
        assert_eq!(byte_offset_to_position(text, 1), Position::new(0, 1));
        assert_eq!(byte_offset_to_position(text, 5), Position::new(0, 3));
        assert_eq!(byte_offset_to_position(text, 7), Position::new(1, 0));
        assert_eq!(
            byte_offset_to_position(text, text.len()),
            Position::new(1, 2)
        );
        assert_eq!(position_to_byte_offset(text, Position::new(0, 3)), Some(5));
        assert_eq!(position_to_byte_offset(text, Position::new(1, 0)), Some(7));
        assert_eq!(position_to_byte_offset(text, Position::new(1, 1)), Some(9));
        assert_eq!(position_to_byte_offset(text, Position::new(0, 2)), None);
    }

    #[test]
    fn initialize_and_requests_expose_full_and_range_semantic_highlighting() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        let initialized = server.handle(request(1, "initialize", json!({})));
        let provider = &initialized.messages[0]["result"]["capabilities"]["semanticTokensProvider"];
        assert_eq!(provider["full"], true);
        assert_eq!(provider["range"], true);
        assert_eq!(provider["legend"]["tokenTypes"][4], "struct");
        assert_eq!(provider["legend"]["tokenModifiers"][9], "defaultLibrary");

        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///main.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "data class User(val name: String)\nfun nameOf(user: User) = user.name"
                }
            }),
        ));
        let full = server.handle(request(
            2,
            "textDocument/semanticTokens/full",
            json!({"textDocument": {"uri": "file:///main.kt"}}),
        ));
        let full_data = full.messages[0]["result"]["data"].as_array().unwrap();
        assert!(!full_data.is_empty());
        assert_eq!(full_data.len() % 5, 0);

        let range = server.handle(request(
            3,
            "textDocument/semanticTokens/range",
            json!({
                "textDocument": {"uri": "file:///main.kt"},
                "range": {
                    "start": {"line": 1, "character": 0},
                    "end": {"line": 2, "character": 0}
                }
            }),
        ));
        let range_data = range.messages[0]["result"]["data"].as_array().unwrap();
        assert!(!range_data.is_empty());
        assert!(range_data.len() < full_data.len());
        assert_eq!(range_data[0], 1);
    }

    #[test]
    fn completion_is_scoped_compiler_backed_and_resolvable() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            calls_for_analyzer.set(calls_for_analyzer.get() + 1);
            super::super::analyze_for_lsp(sources)
        });
        let initialized = server.handle(request(1, "initialize", json!({})));
        let provider = &initialized.messages[0]["result"]["capabilities"]["completionProvider"];
        assert_eq!(provider["resolveProvider"], true);
        assert_eq!(provider["triggerCharacters"], json!(["."]));

        let source = concat!(
            "data class User(val name: String) {\n",
            "  fun greeting(): String = name\n",
            "}\n",
            "fun demo(user: User) {\n",
            "  val local: User = user\n",
            "  user.\n",
            "  val later = 1\n",
            "}\n",
        );
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///main.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": source
                }
            }),
        ));
        assert_eq!(calls.get(), 1);

        let completion = server.handle(request(
            2,
            "textDocument/completion",
            json!({
                "textDocument": {"uri": "file:///main.kt"},
                "position": {"line": 5, "character": 7}
            }),
        ));
        assert_eq!(calls.get(), 1, "completion must use the cached snapshot");
        assert_eq!(completion.messages[0]["result"]["isIncomplete"], false);
        let items = completion.messages[0]["result"]["items"]
            .as_array()
            .unwrap();
        let name = items
            .iter()
            .find(|item| item["label"] == "name")
            .expect("constructor property completion");
        assert_eq!(name["kind"], 10);
        let greeting = items
            .iter()
            .find(|item| item["label"] == "greeting")
            .expect("method completion");
        assert_eq!(greeting["kind"], 2);
        assert!(items.iter().all(|item| item["label"] != "later"));

        let resolved = server.handle(request(3, "completionItem/resolve", greeting.clone()));
        assert_eq!(resolved.messages[0]["result"]["label"], "greeting");
        assert_eq!(
            resolved.messages[0]["result"]["detail"],
            "fun greeting(): String"
        );
        assert_eq!(calls.get(), 1, "resolve must use the cached snapshot");

        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///other.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun unrelated() = 1"
                }
            }),
        ));
        let stale = server.handle(request(4, "completionItem/resolve", greeting.clone()));
        assert!(
            stale.messages[0]["result"]["detail"].is_null(),
            "a source-set refresh must invalidate old positional completion slots"
        );
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn completion_includes_cross_file_top_level_declarations() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            ("file:///Answer.kt", "package demo\nfun answer(): Int = 42"),
            ("file:///Use.kt", "package demo\nfun use(): Int = ans"),
        ] {
            server.handle(notification(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": "kotlin",
                        "version": 1,
                        "text": text
                    }
                }),
            ));
        }

        let completion = server.handle(request(
            2,
            "textDocument/completion",
            json!({
                "textDocument": {"uri": "file:///Use.kt"},
                "position": {"line": 1, "character": 20}
            }),
        ));
        let items = completion.messages[0]["result"]["items"]
            .as_array()
            .unwrap();
        assert!(items
            .iter()
            .any(|item| item["label"] == "answer" && item["kind"] == 3));
    }

    #[test]
    fn document_lifecycle_publishes_diagnostics_and_drops_closed_text() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            calls_for_analyzer.set(calls_for_analyzer.get() + 1);
            sources
                .iter()
                .map(|source| {
                    let diagnostics = if source.contains("bad") {
                        vec![Diagnostic {
                            span: Span::new(0, source.len() as u32),
                            severity: Severity::Error,
                            msg: "bad document".to_string(),
                            file: 0,
                        }]
                    } else {
                        Vec::new()
                    };
                    super::super::DocumentAnalysis::with_diagnostics(diagnostics)
                })
                .collect()
        });

        let initialized = server.handle(request(1, "initialize", json!({})));
        assert_eq!(initialized.messages[0]["id"], 1);
        assert_eq!(
            initialized.messages[0]["result"]["capabilities"]["textDocumentSync"],
            1
        );

        let opened = server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///main.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "bad"
                }
            }),
        ));
        assert_eq!(opened.messages.len(), 1);
        assert_eq!(
            opened.messages[0]["method"],
            "textDocument/publishDiagnostics"
        );
        assert_eq!(opened.messages[0]["params"]["version"], 1);
        assert_eq!(
            opened.messages[0]["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            opened.messages[0]["params"]["diagnostics"][0]["source"],
            "Kotlin"
        );
        assert_eq!(server.open_document_count(), 1);

        let changed = server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///main.kt", "version": 2},
                "contentChanges": [{"text": "fun ok() = 1"}]
            }),
        ));
        assert_eq!(changed.messages[0]["params"]["diagnostics"], json!([]));
        assert_eq!(calls.get(), 2);

        let stale = server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///main.kt", "version": 1},
                "contentChanges": [{"text": "bad again"}]
            }),
        ));
        assert!(stale.messages.is_empty());
        assert_eq!(calls.get(), 2);

        let closed = server.handle(notification(
            "textDocument/didClose",
            json!({"textDocument": {"uri": "file:///main.kt"}}),
        ));
        assert_eq!(closed.messages[0]["params"]["diagnostics"], json!([]));
        assert_eq!(server.open_document_count(), 0);
    }

    #[test]
    fn open_documents_are_analyzed_as_one_source_set() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let unresolved = server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Use.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "package demo\nfun use(): Int = answer()"
                }
            }),
        ));
        assert!(!unresolved.messages[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty());
        let resolved = server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Answer.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "package demo\nfun answer(): Int = 42"
                }
            }),
        ));
        let use_diagnostics = resolved
            .messages
            .iter()
            .find(|message| message["params"]["uri"] == "file:///Use.kt")
            .unwrap();
        assert_eq!(use_diagnostics["params"]["diagnostics"], json!([]));

        let closed = server.handle(notification(
            "textDocument/didClose",
            json!({"textDocument": {"uri": "file:///Answer.kt"}}),
        ));
        let use_diagnostics = closed
            .messages
            .iter()
            .find(|message| message["params"]["uri"] == "file:///Use.kt")
            .unwrap();
        assert!(!use_diagnostics["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn shutdown_then_exit_terminates_cleanly() {
        let mut server = LspService::new(|sources: &[&str]| {
            sources
                .iter()
                .map(|_| super::super::DocumentAnalysis::empty())
                .collect()
        });
        server.handle(request(1, "initialize", json!({})));
        let shutdown = server.handle(request(9, "shutdown", Value::Null));
        assert_eq!(
            shutdown.messages[0],
            json!({"jsonrpc": "2.0", "id": 9, "result": null})
        );
        assert!(!shutdown.exit);

        let exit = server.handle(notification("exit", Value::Null));
        assert!(exit.exit);
        assert_eq!(exit.exit_code, 0);
    }

    #[test]
    fn lifecycle_rejects_requests_outside_the_initialized_session() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            calls_for_analyzer.set(calls_for_analyzer.get() + 1);
            sources
                .iter()
                .map(|_| super::super::DocumentAnalysis::empty())
                .collect()
        });

        let before = server.handle(request(1, "textDocument/hover", json!({})));
        assert_eq!(before.messages[0]["error"]["code"], -32002);
        assert!(server
            .handle(notification("textDocument/didOpen", json!({})))
            .messages
            .is_empty());

        server.handle(request(2, "initialize", json!({})));
        server.handle(request(3, "shutdown", Value::Null));
        let after = server.handle(request(4, "textDocument/hover", json!({})));
        assert_eq!(after.messages[0]["error"]["code"], -32600);
        assert!(server
            .handle(notification("textDocument/didChange", json!({})))
            .messages
            .is_empty());
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn content_length_framing_round_trips_multiple_messages() {
        let first = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let second = br#"{"jsonrpc":"2.0","method":"exit"}"#;
        let mut wire = Vec::new();
        write_framed(&mut wire, first).unwrap();
        write_framed(&mut wire, second).unwrap();

        let mut reader = Cursor::new(wire);
        assert_eq!(
            read_framed(&mut reader, MAX_MESSAGE_BYTES)
                .unwrap()
                .unwrap(),
            first
        );
        assert_eq!(
            read_framed(&mut reader, MAX_MESSAGE_BYTES)
                .unwrap()
                .unwrap(),
            second
        );
        assert!(read_framed(&mut reader, MAX_MESSAGE_BYTES)
            .unwrap()
            .is_none());
    }

    #[test]
    fn framing_rejects_oversized_message_before_reading_body() {
        let wire = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_BYTES + 1);
        let error = read_framed(&mut Cursor::new(wire), MAX_MESSAGE_BYTES).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("too large"));
    }

    #[test]
    fn framing_bounds_a_header_line_without_a_newline() {
        let mut wire = Cursor::new(vec![b'x'; 2 * MAX_HEADER_BYTES]);
        let error = read_framed(&mut wire, MAX_MESSAGE_BYTES).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("header too large"));
        assert!(
            wire.position() <= (MAX_HEADER_BYTES + 1) as u64,
            "reader consumed an unbounded header before rejecting it"
        );
    }

    #[test]
    fn queued_changes_are_coalesced_to_the_latest_text() {
        let first = notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///main.kt", "version": 2},
                "contentChanges": [{"text": "two"}]
            }),
        );
        let latest = notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///main.kt", "version": 3},
                "contentChanges": [{"text": "three"}]
            }),
        );
        let following = request(9, "textDocument/hover", json!({}));
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        sender.send(Incoming::Message(latest)).unwrap();
        sender.send(Incoming::Message(following.clone())).unwrap();
        let mut pending = std::collections::VecDeque::new();

        let coalesced = coalesce_document_notifications(first, &receiver, &mut pending);
        assert_eq!(coalesced.len(), 1);
        assert_eq!(coalesced[0]["params"]["textDocument"]["version"], 3);
        let Incoming::Message(pending_message) = pending.pop_front().unwrap() else {
            panic!("following request was not preserved");
        };
        assert_eq!(pending_message, following);
    }

    #[test]
    fn queued_changes_for_multiple_documents_form_one_batch() {
        let first = notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///a.kt", "version": 2},
                "contentChanges": [{"text": "a2"}]
            }),
        );
        let second = notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///b.kt", "version": 2},
                "contentChanges": [{"text": "b2"}]
            }),
        );
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        sender.send(Incoming::Message(second)).unwrap();
        let mut pending = std::collections::VecDeque::new();

        let changes = coalesce_document_notifications(first, &receiver, &mut pending);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0]["params"]["textDocument"]["uri"], "file:///a.kt");
        assert_eq!(changes[1]["params"]["textDocument"]["uri"], "file:///b.kt");
    }

    #[test]
    fn a_multi_document_change_batch_runs_analysis_once() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            calls_for_analyzer.set(calls_for_analyzer.get() + 1);
            sources
                .iter()
                .map(|_| super::super::DocumentAnalysis::empty())
                .collect()
        });
        server.handle(request(1, "initialize", json!({})));
        for uri in ["file:///a.kt", "file:///b.kt"] {
            server.handle(notification(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": "kotlin",
                        "version": 1,
                        "text": "fun value() = 1"
                    }
                }),
            ));
        }
        assert_eq!(calls.get(), 2);

        let changes = ["file:///a.kt", "file:///b.kt"]
            .into_iter()
            .map(|uri| {
                notification(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": uri, "version": 2},
                        "contentChanges": [{"text": "fun value() = 2"}]
                    }),
                )
            })
            .collect();
        let mut output = Vec::new();
        assert!(
            super::implementation::dispatch_document_batch(&mut output, &mut server, changes)
                .unwrap()
                .is_none()
        );
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn open_and_close_batches_each_run_analysis_once() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            calls_for_analyzer.set(calls_for_analyzer.get() + 1);
            sources
                .iter()
                .map(|_| super::super::DocumentAnalysis::empty())
                .collect()
        });
        server.handle(request(1, "initialize", json!({})));
        let opens = ["file:///a.kt", "file:///b.kt"]
            .into_iter()
            .map(|uri| {
                notification(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": "kotlin",
                            "version": 1,
                            "text": "fun value() = 1"
                        }
                    }),
                )
            })
            .collect();
        let mut output = Vec::new();
        super::implementation::dispatch_document_batch(&mut output, &mut server, opens).unwrap();
        assert_eq!(calls.get(), 1);

        let closes = ["file:///a.kt", "file:///b.kt"]
            .into_iter()
            .map(|uri| {
                notification(
                    "textDocument/didClose",
                    json!({"textDocument": {"uri": uri}}),
                )
            })
            .collect();
        super::implementation::dispatch_document_batch(&mut output, &mut server, closes).unwrap();
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn connection_runs_real_compiler_analysis_until_clean_exit() {
        let messages = [
            request(1, "initialize", json!({})),
            notification(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": "file:///main.kt",
                        "languageId": "kotlin",
                        "version": 7,
                        "text": "fun box(): Int = \"no\""
                    }
                }),
            ),
            request(2, "shutdown", Value::Null),
            notification("exit", Value::Null),
        ];
        let mut input = Vec::new();
        for message in messages {
            write_framed(&mut input, serde_json::to_vec(&message).unwrap().as_slice()).unwrap();
        }

        let mut output = Vec::new();
        assert_eq!(
            run_connection(&mut Cursor::new(input), &mut output).unwrap(),
            0
        );

        let mut output = Cursor::new(output);
        let initialize: Value = serde_json::from_slice(
            &read_framed(&mut output, MAX_MESSAGE_BYTES)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let diagnostics: Value = serde_json::from_slice(
            &read_framed(&mut output, MAX_MESSAGE_BYTES)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let shutdown: Value = serde_json::from_slice(
            &read_framed(&mut output, MAX_MESSAGE_BYTES)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(initialize["id"], 1);
        assert_eq!(diagnostics["params"]["version"], 7);
        assert_eq!(
            diagnostics["params"]["diagnostics"][0]["message"],
            "Return type mismatch: expected 'Int', actual 'String'."
        );
        assert_eq!(shutdown["id"], 2);
        assert!(read_framed(&mut output, MAX_MESSAGE_BYTES)
            .unwrap()
            .is_none());
    }

    #[test]
    fn connection_accepts_injected_analysis_provider() {
        let initialize = request(1, "initialize", json!({}));
        let open = notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///main.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "anything"
                }
            }),
        );
        let exit = notification("exit", Value::Null);
        let mut input = Vec::new();
        write_framed(&mut input, &serde_json::to_vec(&initialize).unwrap()).unwrap();
        write_framed(&mut input, &serde_json::to_vec(&open).unwrap()).unwrap();
        write_framed(&mut input, &serde_json::to_vec(&exit).unwrap()).unwrap();

        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut output = Vec::new();
        let exit_code = run_connection_with(
            &mut Cursor::new(input),
            &mut output,
            move |sources: &[&str]| {
                calls_for_analyzer.set(calls_for_analyzer.get() + 1);
                sources
                    .iter()
                    .map(|_| super::super::DocumentAnalysis::empty())
                    .collect()
            },
        )
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(exit_code, 1, "exit without shutdown is an LSP failure");
    }

    #[test]
    fn hover_uses_cached_compact_analysis() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            calls_for_analyzer.set(calls_for_analyzer.get() + 1);
            super::super::analyze_for_lsp(sources)
        });
        server.handle(request(1, "initialize", json!({})));
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///main.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun box(): Int { val answer = 42; return answer }"
                }
            }),
        ));

        let hover = server.handle(request(
            2,
            "textDocument/hover",
            json!({
                "textDocument": {"uri": "file:///main.kt"},
                "position": {"line": 0, "character": 43}
            }),
        ));
        assert_eq!(calls.get(), 1, "hover must not rerun compiler analysis");
        assert_eq!(hover.messages[0]["id"], 2);
        assert_eq!(
            hover.messages[0]["result"]["contents"],
            json!({"kind": "plaintext", "value": "Int"})
        );
    }
}
