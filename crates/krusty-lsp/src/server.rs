mod implementation;
pub use implementation::*;

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::io::Cursor;
    use std::rc::Rc;

    use serde_json::{json, Value};

    use super::*;
    use crate::DocumentAnalysis;
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

    #[derive(Default)]
    struct RecordingHost {
        root: Option<std::path::PathBuf>,
        globs: Vec<String>,
        refreshes: u32,
        pending: bool,
        feedback_reanalyze: bool,
        feedback_message: Option<(ProjectMessageKind, String)>,
    }

    impl Analysis for RecordingHost {
        fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
            sources.iter().map(|_| DocumentAnalysis::empty()).collect()
        }

        fn set_workspace_root(&mut self, root: Option<std::path::PathBuf>) {
            self.root = root;
        }

        fn watched_globs(&mut self) -> Vec<String> {
            self.globs.clone()
        }

        fn note_project_change(&mut self) {
            self.pending = true;
        }

        fn project_refresh_due_in(&self) -> Option<std::time::Duration> {
            self.pending.then(std::time::Duration::default)
        }

        fn refresh_project(&mut self) -> ProjectFeedback {
            self.pending = false;
            self.refreshes += 1;
            ProjectFeedback {
                reanalyze: self.feedback_reanalyze,
                message: self.feedback_message.take(),
            }
        }
    }

    #[test]
    fn initialized_registers_a_file_watcher_for_the_backend_globs() {
        let host = RecordingHost {
            globs: vec!["**/*.gradle.kts".to_string(), "**/pom.xml".to_string()],
            ..RecordingHost::default()
        };
        let mut server = LspService::new(host);
        server.handle(request(1, "initialize", json!({})));

        let dispatch = server.handle(notification("initialized", json!({})));
        let registration = &dispatch.messages[0];
        assert_eq!(registration["method"], "client/registerCapability");
        let watcher = &registration["params"]["registrations"][0];
        assert_eq!(watcher["method"], "workspace/didChangeWatchedFiles");
        assert_eq!(
            watcher["registerOptions"]["watchers"][0]["globPattern"],
            "**/*.gradle.kts"
        );
    }

    #[test]
    fn a_backend_without_globs_registers_no_watcher() {
        let mut server = LspService::new(RecordingHost::default());
        server.handle(request(1, "initialize", json!({})));
        assert!(server
            .handle(notification("initialized", json!({})))
            .messages
            .is_empty());
    }

    #[test]
    fn a_watched_file_change_defers_the_refresh_then_shows_the_status_message_when_due() {
        let host = RecordingHost {
            feedback_message: Some((ProjectMessageKind::Warning, "sync failed".to_string())),
            ..RecordingHost::default()
        };
        let mut server = LspService::new(host);
        server.handle(request(1, "initialize", json!({})));
        server.handle(notification("initialized", json!({})));

        let dispatch = server.handle(notification(
            "workspace/didChangeWatchedFiles",
            json!({ "changes": [{ "uri": "file:///p/build.gradle.kts", "type": 2 }] }),
        ));
        assert!(dispatch.messages.is_empty());
        assert_eq!(
            server.project_refresh_due_in(),
            Some(std::time::Duration::ZERO)
        );

        let messages = server.run_due_project_refresh();
        assert_eq!(messages[0]["method"], "window/showMessage");
        assert_eq!(messages[0]["params"]["type"], 2);
        assert_eq!(messages[0]["params"]["message"], "sync failed");
        assert_eq!(server.project_refresh_due_in(), None);
    }

    #[test]
    fn a_project_change_reanalyzes_open_documents_when_the_refresh_runs() {
        let host = RecordingHost {
            feedback_reanalyze: true,
            ..RecordingHost::default()
        };
        let mut server = LspService::new(host);
        server.handle(request(1, "initialize", json!({})));
        server.handle(notification("initialized", json!({})));
        server.handle(notification(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": "file:///p/A.kt", "languageId": "kotlin", "version": 1, "text": "fun a() {}"
            }}),
        ));

        server.handle(notification(
            "workspace/didChangeWatchedFiles",
            json!({ "changes": [{ "uri": "file:///p/build.gradle.kts", "type": 2 }] }),
        ));
        let messages = server.run_due_project_refresh();
        assert!(messages.iter().any(|message| {
            message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == "file:///p/A.kt"
        }));
    }

    #[test]
    fn a_response_to_a_server_request_is_ignored_rather_than_answered() {
        let mut server = LspService::new(RecordingHost::default());
        server.handle(request(1, "initialize", json!({})));
        let dispatch = server.handle(json!({
            "jsonrpc": "2.0",
            "id": "krusty/registerWatchers",
            "result": null,
        }));
        assert!(dispatch.messages.is_empty());
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
    fn document_symbols_match_official_hierarchy_kinds_and_ranges() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        let initialized = server.handle(request(1, "initialize", json!({})));
        assert_eq!(
            initialized.messages[0]["result"]["capabilities"]["documentSymbolProvider"],
            true
        );
        let uri = "file:///DocumentSymbols.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "val topValue: Int = 1\n\
                             fun topFunction(arg: Int): Int = arg\n\
                             class Box(val item: Int) {\n\
                             \u{20}\u{20}var mutable: String = \"\"\n\
                             \u{20}\u{20}fun member(value: Int): Int = value\n\
                             }\n"
                }
            }),
        ));

        let response = server.handle(request(
            2,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([
                {
                    "name": "topValue",
                    "kind": 7,
                    "deprecated": false,
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 21}
                    },
                    "selectionRange": {
                        "start": {"line": 0, "character": 4},
                        "end": {"line": 0, "character": 12}
                    }
                },
                {
                    "name": "topFunction",
                    "kind": 12,
                    "deprecated": false,
                    "range": {
                        "start": {"line": 1, "character": 0},
                        "end": {"line": 1, "character": 36}
                    },
                    "selectionRange": {
                        "start": {"line": 1, "character": 4},
                        "end": {"line": 1, "character": 15}
                    }
                },
                {
                    "name": "Box",
                    "kind": 5,
                    "deprecated": false,
                    "range": {
                        "start": {"line": 2, "character": 0},
                        "end": {"line": 5, "character": 1}
                    },
                    "selectionRange": {
                        "start": {"line": 2, "character": 6},
                        "end": {"line": 2, "character": 9}
                    },
                    "children": [
                        {
                            "name": "item",
                            "kind": 13,
                            "deprecated": false,
                            "range": {
                                "start": {"line": 2, "character": 10},
                                "end": {"line": 2, "character": 23}
                            },
                            "selectionRange": {
                                "start": {"line": 2, "character": 14},
                                "end": {"line": 2, "character": 18}
                            }
                        },
                        {
                            "name": "Box",
                            "kind": 9,
                            "deprecated": false,
                            "range": {
                                "start": {"line": 2, "character": 9},
                                "end": {"line": 2, "character": 24}
                            },
                            "selectionRange": {
                                "start": {"line": 2, "character": 9},
                                "end": {"line": 2, "character": 24}
                            }
                        },
                        {
                            "name": "mutable",
                            "kind": 7,
                            "deprecated": false,
                            "range": {
                                "start": {"line": 3, "character": 2},
                                "end": {"line": 3, "character": 26}
                            },
                            "selectionRange": {
                                "start": {"line": 3, "character": 6},
                                "end": {"line": 3, "character": 13}
                            }
                        },
                        {
                            "name": "member",
                            "kind": 6,
                            "deprecated": false,
                            "range": {
                                "start": {"line": 4, "character": 2},
                                "end": {"line": 4, "character": 37}
                            },
                            "selectionRange": {
                                "start": {"line": 4, "character": 6},
                                "end": {"line": 4, "character": 12}
                            }
                        }
                    ]
                }
            ])
        );
    }

    #[test]
    fn document_symbols_are_cached_and_include_semicolon_declaration_ranges() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            calls_for_analyzer.set(calls_for_analyzer.get() + 1);
            super::super::analyze_for_lsp(sources)
        });
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///SemicolonSymbols.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "object Registry { val size: Int = 1; fun clear() {} }\n"
                }
            }),
        ));

        let response = server.handle(request(
            2,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        ));

        assert_eq!(calls.get(), 1, "document symbols must not rerun analysis");
        assert_eq!(
            response.messages[0]["result"][0]["children"][0]["range"],
            json!({
                "start": {"line": 0, "character": 18},
                "end": {"line": 0, "character": 36}
            })
        );
    }

    #[test]
    fn document_symbols_preserve_companion_hierarchy_and_exact_locations() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///CompanionSymbols.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class Container {\n\
                             \u{20}\u{20}companion object Factory {\n\
                             \u{20}\u{20}\u{20}\u{20}val answer: Int = 42\n\
                             \u{20}\u{20}\u{20}\u{20}fun create(): Container = Container()\n\
                             \u{20}\u{20}}\n\
                             \u{20}\u{20}fun outer(): Int = 1\n\
                             }\n"
                }
            }),
        ));

        let response = server.handle(request(
            2,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        ));
        assert_eq!(
            response.messages[0]["result"][0]["children"][0],
            json!({
                "name": "Factory",
                "kind": 19,
                "deprecated": false,
                "range": {
                    "start": {"line": 1, "character": 2},
                    "end": {"line": 4, "character": 3}
                },
                "selectionRange": {
                    "start": {"line": 1, "character": 19},
                    "end": {"line": 1, "character": 26}
                },
                "children": [
                    {
                        "name": "answer",
                        "kind": 7,
                        "deprecated": false,
                        "range": {
                            "start": {"line": 2, "character": 4},
                            "end": {"line": 2, "character": 24}
                        },
                        "selectionRange": {
                            "start": {"line": 2, "character": 8},
                            "end": {"line": 2, "character": 14}
                        }
                    },
                    {
                        "name": "create",
                        "kind": 6,
                        "deprecated": false,
                        "range": {
                            "start": {"line": 3, "character": 4},
                            "end": {"line": 3, "character": 41}
                        },
                        "selectionRange": {
                            "start": {"line": 3, "character": 8},
                            "end": {"line": 3, "character": 14}
                        }
                    }
                ]
            })
        );
    }

    #[test]
    fn document_symbols_match_data_class_and_typealias_kinds_and_locations() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///KindSymbols.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "data class Record(val value: Int)\n\
                             typealias Alias = Record\n"
                }
            }),
        ));

        let response = server.handle(request(
            2,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        ));
        let symbols = response.messages[0]["result"].as_array().unwrap();
        assert_eq!(symbols[0]["kind"], 23);
        assert_eq!(
            symbols[0]["range"],
            json!({
                "start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 33}
            })
        );
        assert_eq!(
            symbols[1],
            json!({
                "name": "Alias",
                "kind": 5,
                "deprecated": false,
                "range": {
                    "start": {"line": 1, "character": 0},
                    "end": {"line": 1, "character": 24}
                },
                "selectionRange": {
                    "start": {"line": 1, "character": 10},
                    "end": {"line": 1, "character": 15}
                }
            })
        );
    }

    #[test]
    fn document_symbols_match_secondary_constructor_and_unnamed_companion_locations() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///ConstructorSymbols.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class Secondary {\n\
                             \u{20}\u{20}constructor(value: Int) { println(value) }\n\
                             }\n\
                             class DefaultCompanion {\n\
                             \u{20}\u{20}companion object { val only: Int = 1 }\n\
                             }\n"
                }
            }),
        ));

        let response = server.handle(request(
            2,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        ));
        let symbols = response.messages[0]["result"].as_array().unwrap();
        assert_eq!(
            symbols[0]["children"][0],
            json!({
                "name": "Secondary",
                "kind": 9,
                "deprecated": false,
                "range": {
                    "start": {"line": 1, "character": 2},
                    "end": {"line": 1, "character": 44}
                },
                "selectionRange": {
                    "start": {"line": 1, "character": 2},
                    "end": {"line": 1, "character": 44}
                }
            })
        );
        assert_eq!(
            symbols[1]["children"][0]["selectionRange"],
            json!({
                "start": {"line": 4, "character": 2},
                "end": {"line": 4, "character": 40}
            })
        );
        assert_eq!(
            symbols[1]["children"][0]["children"][0]["range"],
            json!({
                "start": {"line": 4, "character": 21},
                "end": {"line": 4, "character": 38}
            })
        );
    }

    #[test]
    fn document_symbols_do_not_borrow_following_headers_or_leaked_annotations() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///AdjacentSymbols.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "annotation class Marker\n\
                             data class Record(val value: Int)\n\
                             @Deprecated(\"old alias\")\n\
                             typealias OldAlias = Record\n\
                             class Injected @Deprecated(\"old constructor\") constructor(val value: Int)\n\
                             class DefaultCompanion {\n\
                             \u{20}\u{20}@Deprecated(\"old companion\")\n\
                             \u{20}\u{20}companion object {}\n\
                             }\n\
                             class Fresh\n"
                }
            }),
        ));

        let response = server.handle(request(
            2,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        ));
        let symbols = response.messages[0]["result"].as_array().unwrap();
        let symbol = |name: &str| {
            symbols
                .iter()
                .find(|symbol| symbol["name"] == name)
                .unwrap_or_else(|| panic!("{name} symbol"))
        };
        assert!(symbol("Marker").get("children").is_none());
        assert!(symbol("Fresh").get("children").is_none());
        assert_eq!(symbol("Injected")["deprecated"], false);
        assert_eq!(symbol("Injected")["children"][1]["deprecated"], true);
        assert_eq!(symbol("DefaultCompanion")["deprecated"], false);
        assert_eq!(
            symbol("DefaultCompanion")["children"][0]["deprecated"],
            true
        );
    }

    #[test]
    fn document_symbols_mark_deprecated_property_without_leaking_to_the_next_class() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///DeprecatedPropertySymbols.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "@Deprecated(\n\
                             \u{20}\u{20}\"old\"\n\
                             )\n\
                             val oldProperty: Int = 1\n\
                             @Other(Deprecated)\n\
                             val currentProperty: Int = 2\n\
                             class Fresh\n"
                }
            }),
        ));

        let response = server.handle(request(
            2,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        ));
        let symbols = response.messages[0]["result"].as_array().unwrap();
        assert_eq!(symbols[0]["deprecated"], true);
        assert_eq!(symbols[0]["tags"], json!([1]));
        assert_eq!(
            symbols[0]["range"],
            json!({
                "start": {"line": 0, "character": 0},
                "end": {"line": 3, "character": 24}
            })
        );
        assert_eq!(
            symbols[0]["selectionRange"],
            json!({
                "start": {"line": 3, "character": 4},
                "end": {"line": 3, "character": 15}
            })
        );
        assert_eq!(symbols[1]["name"], "currentProperty");
        assert_eq!(symbols[1]["deprecated"], false);
        assert!(symbols[1].get("tags").is_none());
        assert_eq!(
            symbols[1]["range"],
            json!({
                "start": {"line": 4, "character": 0},
                "end": {"line": 5, "character": 28}
            })
        );
        assert_eq!(symbols[2]["name"], "Fresh");
        assert_eq!(symbols[2]["deprecated"], false);
        assert!(symbols[2].get("tags").is_none());
    }

    #[test]
    fn document_symbols_balance_enum_arguments_and_select_explicit_constructor_range() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///BalancedSymbolRanges.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class Injected @Deprecated(\"old constructor\") constructor(val injected: Int)\n\
                             enum class Pair(val left: Int, val right: Int) { BOTH(1, 2) }\n"
                }
            }),
        ));

        let response = server.handle(request(
            2,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        ));
        let symbols = response.messages[0]["result"].as_array().unwrap();
        assert_eq!(symbols[0]["deprecated"], false);
        assert_eq!(
            symbols[0]["children"][0]["range"],
            json!({
                "start": {"line": 0, "character": 58},
                "end": {"line": 0, "character": 75}
            })
        );
        assert_eq!(
            symbols[0]["children"][1],
            json!({
                "name": "Injected",
                "kind": 9,
                "deprecated": true,
                "tags": [1],
                "range": {
                    "start": {"line": 0, "character": 15},
                    "end": {"line": 0, "character": 76}
                },
                "selectionRange": {
                    "start": {"line": 0, "character": 15},
                    "end": {"line": 0, "character": 76}
                }
            })
        );
        assert_eq!(
            symbols[1]["children"][3]["range"],
            json!({
                "start": {"line": 1, "character": 49},
                "end": {"line": 1, "character": 59}
            })
        );
        assert_eq!(
            symbols[1]["children"][3]["selectionRange"],
            json!({
                "start": {"line": 1, "character": 49},
                "end": {"line": 1, "character": 53}
            })
        );
    }

    #[test]
    fn definition_matches_official_class_parameter_and_property_ranges() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            calls_for_analyzer.set(calls_for_analyzer.get() + 1);
            super::super::analyze_for_lsp(sources)
        });
        let initialized = server.handle(request(1, "initialize", json!({})));
        assert_eq!(
            initialized.messages[0]["result"]["capabilities"]["definitionProvider"],
            true
        );

        let uri = "file:///BasicTokens.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "data class User(val name: String)\n\
                             fun greet(user: User): String = user.name\n"
                }
            }),
        ));
        assert_eq!(calls.get(), 1);

        for (id, line, character, target_line, target_start, target_end) in [
            (2, 1, 17, 0, 11, 15),
            (3, 1, 33, 1, 10, 14),
            (4, 1, 38, 0, 20, 24),
        ] {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": line, "character": character}
                }),
            ));
            assert_eq!(
                response.messages[0]["result"],
                json!([{
                    "uri": uri,
                    "range": {
                        "start": {"line": target_line, "character": target_start},
                        "end": {"line": target_line, "character": target_end}
                    }
                }])
            );
        }
        assert_eq!(
            calls.get(),
            1,
            "definition requests must use compact cached spans"
        );
    }

    #[test]
    fn incomplete_refresh_clears_stale_definition_snapshots() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            let call = calls_for_analyzer.get();
            calls_for_analyzer.set(call + 1);
            if call == 0 {
                super::super::analyze_for_lsp(sources)
            } else {
                Vec::new()
            }
        });
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///Stale.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun target(): Int = 1\nfun use(): Int = target()\n"
                }
            }),
        ));
        server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{
                    "text": "fun banana(): Int = 1\nfun use(): Int = absent()\n"
                }]
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 18}
            }),
        ));
        assert_eq!(response.messages[0]["result"], json!([]));
    }

    #[test]
    fn definition_resolves_an_exact_cross_file_function_location() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            calls_for_analyzer.set(calls_for_analyzer.get() + 1);
            super::super::analyze_for_lsp(sources)
        });
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            (
                "file:///DefinitionTarget.kt",
                "package demo\nfun answer(): Int = 42\n",
            ),
            (
                "file:///DefinitionUse.kt",
                "package demo\nfun use(): Int = answer()\n",
            ),
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
        assert_eq!(calls.get(), 2);

        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///DefinitionUse.kt"},
                "position": {"line": 1, "character": 18}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": "file:///DefinitionTarget.kt",
                "range": {
                    "start": {"line": 1, "character": 4},
                    "end": {"line": 1, "character": 10}
                }
            }])
        );
        assert_eq!(calls.get(), 2, "definition must not rerun analysis");
    }

    #[test]
    fn references_match_exact_cross_file_ranges_and_declaration_filtering() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_analyzer = calls.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            calls_for_analyzer.set(calls_for_analyzer.get() + 1);
            super::super::analyze_for_lsp(sources)
        });
        let initialized = server.handle(request(1, "initialize", json!({})));
        assert_eq!(
            initialized.messages[0]["result"]["capabilities"]["referencesProvider"],
            true
        );
        for (uri, text) in [
            (
                "file:///DefinitionTarget.kt",
                "package demo\nfun answer(): Int = 42\n",
            ),
            (
                "file:///DefinitionUse.kt",
                "package demo\nfun use(): Int = answer()\n",
            ),
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
        assert_eq!(calls.get(), 2);

        for (id, include_declaration, expected) in [
            (
                2,
                true,
                json!([
                    {
                        "uri": "file:///DefinitionTarget.kt",
                        "range": {
                            "start": {"line": 1, "character": 4},
                            "end": {"line": 1, "character": 10}
                        }
                    },
                    {
                        "uri": "file:///DefinitionUse.kt",
                        "range": {
                            "start": {"line": 1, "character": 17},
                            "end": {"line": 1, "character": 23}
                        }
                    }
                ]),
            ),
            (
                3,
                false,
                json!([
                    {
                        "uri": "file:///DefinitionUse.kt",
                        "range": {
                            "start": {"line": 1, "character": 17},
                            "end": {"line": 1, "character": 23}
                        }
                    }
                ]),
            ),
        ] {
            let response = server.handle(request(
                id,
                "textDocument/references",
                json!({
                    "textDocument": {"uri": "file:///DefinitionUse.kt"},
                    "position": {"line": 1, "character": 18},
                    "context": {"includeDeclaration": include_declaration}
                }),
            ));
            assert_eq!(response.messages[0]["result"], expected);
        }
        assert_eq!(calls.get(), 2, "references must use compact cached spans");
    }

    #[test]
    fn references_keep_checker_selected_overloads_separate() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///Overloads.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun pick(value: Int): Int = value\n\
                             fun pick(value: String): Int = value.length\n\
                             fun use(): Int = pick(1)\n"
                }
            }),
        ));

        let response = server.handle(request(
            2,
            "textDocument/references",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 2, "character": 18},
                "context": {"includeDeclaration": true}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([
                {
                    "uri": uri,
                    "range": {
                        "start": {"line": 0, "character": 4},
                        "end": {"line": 0, "character": 8}
                    }
                },
                {
                    "uri": uri,
                    "range": {
                        "start": {"line": 2, "character": 17},
                        "end": {"line": 2, "character": 21}
                    }
                }
            ])
        );
    }

    #[test]
    fn definition_resolves_a_selected_source_extension_function() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///Extension.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class C\n\
                             fun C.ext(x: Int): Int = x\n\
                             fun use(c: C): Int = c.ext(1)\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 2, "character": 24}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 1, "character": 6},
                    "end": {"line": 1, "character": 9}
                }
            }])
        );
    }

    #[test]
    fn definition_uses_the_selected_cross_file_extension_overload() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            (
                "file:///IntExtension.kt",
                "package demo\nclass C\nfun C.pick(): Int = 0\n",
            ),
            (
                "file:///StringExtension.kt",
                "package demo\nfun C.pick(value: String): Int = value.length\n",
            ),
            (
                "file:///Use.kt",
                "package demo\nfun use(c: C): Int = c.pick(\"x\")\n",
            ),
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

        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///Use.kt"},
                "position": {"line": 1, "character": 26}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": "file:///StringExtension.kt",
                "range": {
                    "start": {"line": 1, "character": 6},
                    "end": {"line": 1, "character": 10}
                }
            }])
        );
    }

    #[test]
    fn definition_resolves_a_source_extension_property() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///ExtensionProperty.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class C\n\
                             val C.ext: Int get() = 1\n\
                             fun use(c: C): Int = c.ext\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 2, "character": 24}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 1, "character": 6},
                    "end": {"line": 1, "character": 9}
                }
            }])
        );
    }

    #[test]
    fn definition_resolves_a_generic_source_extension() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///GenericExtension.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun <T> T.identity(): T = this\n\
                             fun use(): Int = 1.identity()\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 20}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 0, "character": 10},
                    "end": {"line": 0, "character": 18}
                }
            }])
        );
    }

    #[test]
    fn definition_does_not_select_an_unimported_source_extension() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            (
                "file:///SourceExtension.kt",
                "package a\nfun String.reversed(): String = this\n",
            ),
            (
                "file:///LibraryUse.kt",
                "package b\nfun use(): String = \"ab\".reversed()\n",
            ),
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
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///LibraryUse.kt"},
                "position": {"line": 1, "character": 26}
            }),
        ));
        assert_eq!(response.messages[0]["result"], json!([]));
    }

    #[test]
    fn definition_does_not_treat_an_extension_as_a_receiverless_function() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///ReceiverlessExtension.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun String.ext(): Int = 1\n\
                             fun use(): Int = ext()\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 17}
            }),
        ));
        assert_eq!(response.messages[0]["result"], json!([]));
    }

    #[test]
    fn definition_prefers_local_values_and_functions() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///Locals.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun local(): Int {\n\
                                 \u{20}\u{20}\u{20}\u{20}val answer = 40\n\
                                 \u{20}\u{20}\u{20}\u{20}fun nested(): Int = 2\n\
                                 \u{20}\u{20}\u{20}\u{20}return answer + nested()\n\
                                 }\n"
                }
            }),
        ));

        for (id, character, target_line) in [(2, 12, 1), (3, 21, 2)] {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 3, "character": character}
                }),
            ));
            assert_eq!(
                response.messages[0]["result"],
                json!([{
                    "uri": uri,
                    "range": {
                        "start": {"line": target_line, "character": 8},
                        "end": {"line": target_line, "character": 14}
                    }
                }])
            );
        }
    }

    #[test]
    fn definition_distinguishes_same_named_local_values_and_functions() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///LocalKinds.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun use(): Int {\n\
                             \u{20}\u{20}fun size(): Int = 2\n\
                             \u{20}\u{20}val size: Int = 1\n\
                             \u{20}\u{20}return size + size()\n\
                             }\n"
                }
            }),
        ));

        for (id, character, target_line) in [(2, 9, 2), (3, 16, 1)] {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 3, "character": character}
                }),
            ));
            assert_eq!(
                response.messages[0]["result"],
                json!([{
                    "uri": uri,
                    "range": {
                        "start": {"line": target_line, "character": 6},
                        "end": {"line": target_line, "character": 10}
                    }
                }])
            );
        }
    }

    #[test]
    fn definition_uses_the_checker_selected_overload() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///Overloads.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun select(value: Int): Int = value\n\
                             fun select(value: String): Int = value.length\n\
                             fun choose(): Int = select(1)\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 2, "character": 21}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 0, "character": 4},
                    "end": {"line": 0, "character": 10}
                }
            }])
        );
    }

    #[test]
    fn definition_distinguishes_cross_file_top_level_values_and_functions() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            (
                "file:///Definitions.kt",
                "package p\nval size: Int = 1\nfun size(): Int = 2\n",
            ),
            (
                "file:///Use.kt",
                "package p\nfun use(): Int = size + size()\n",
            ),
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

        for (id, character, target_line) in [(2, 17, 1), (3, 24, 2)] {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": "file:///Use.kt"},
                    "position": {"line": 1, "character": character}
                }),
            ));
            assert_eq!(
                response.messages[0]["result"],
                json!([{
                    "uri": "file:///Definitions.kt",
                    "range": {
                        "start": {"line": target_line, "character": 4},
                        "end": {"line": target_line, "character": 8}
                    }
                }])
            );
        }
    }

    #[test]
    fn definition_uses_the_checker_selected_member_overload() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///MemberOverloads.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class Choices {\n\
                             \u{20}\u{20}fun select(value: Int): Int = value\n\
                             \u{20}\u{20}fun select(value: String): Int = value.length\n\
                             }\n\
                             fun choose(c: Choices): Int = c.select(1)\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 4, "character": 33}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 1, "character": 6},
                    "end": {"line": 1, "character": 12}
                }
            }])
        );
    }

    #[test]
    fn definition_distinguishes_a_property_from_a_zero_argument_method() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///MemberKinds.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class Sized {\n\
                             \u{20}\u{20}val size: Int = 1\n\
                             \u{20}\u{20}fun size(): Int = 2\n\
                             }\n\
                             fun use(c: Sized): Int = c.size() + c.size\n"
                }
            }),
        ));

        for (id, character, target_line) in [(2, 28, 2), (3, 39, 1)] {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 4, "character": character}
                }),
            ));
            assert_eq!(
                response.messages[0]["result"],
                json!([{
                    "uri": uri,
                    "range": {
                        "start": {"line": target_line, "character": 6},
                        "end": {"line": target_line, "character": 10}
                    }
                }])
            );
        }
    }

    #[test]
    fn definition_distinguishes_instance_and_companion_members() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///MemberStaticness.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class Mixed {\n\
                             \u{20}\u{20}fun pick(): Int = 1\n\
                             \u{20}\u{20}companion object {\n\
                             \u{20}\u{20}\u{20}\u{20}fun pick(): Int = 2\n\
                             \u{20}\u{20}}\n\
                             }\n\
                             fun use(m: Mixed): Int = m.pick() + Mixed.pick()\n"
                }
            }),
        ));

        for (id, character, target_line, target_start) in [(2, 29, 1, 6), (3, 44, 3, 8)] {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 6, "character": character}
                }),
            ));
            assert_eq!(
                response.messages[0]["result"],
                json!([{
                    "uri": uri,
                    "range": {
                        "start": {"line": target_line, "character": target_start},
                        "end": {"line": target_line, "character": target_start + 4}
                    }
                }])
            );
        }
    }

    #[test]
    fn definition_resolves_object_members_as_instance_members() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///ObjectMembers.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "object Obj {\n\
                             \u{20}\u{20}val prop: Int = 1\n\
                             \u{20}\u{20}fun pick(): Int = prop\n\
                             }\n\
                             fun use(): Int = Obj.pick() + Obj.prop\n"
                }
            }),
        ));

        for (id, character, target_line, target_end) in [(2, 22, 2, 10), (3, 35, 1, 10)] {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 4, "character": character}
                }),
            ));
            assert_eq!(
                response.messages[0]["result"],
                json!([{
                    "uri": uri,
                    "range": {
                        "start": {"line": target_line, "character": 6},
                        "end": {"line": target_line, "character": target_end}
                    }
                }])
            );
        }
    }

    #[test]
    fn definition_uses_the_companion_target_for_an_unqualified_companion_call() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///CompanionScope.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class Mixed {\n\
                             \u{20}\u{20}fun pick(): Int = 2\n\
                             \u{20}\u{20}companion object {\n\
                             \u{20}\u{20}\u{20}\u{20}fun pick(): Int = 1\n\
                             \u{20}\u{20}\u{20}\u{20}fun call(): Int = pick()\n\
                             \u{20}\u{20}}\n\
                             }\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 4, "character": 23}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 3, "character": 8},
                    "end": {"line": 3, "character": 12}
                }
            }])
        );
    }

    #[test]
    fn definition_resolves_inherited_source_members() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///Inherited.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "open class Base {\n\
                             \u{20}\u{20}fun inherited(): Int = 1\n\
                             \u{20}\u{20}val value: Int = 2\n\
                             }\n\
                             class Child : Base()\n\
                             fun use(c: Child): Int = c.inherited() + c.value\n"
                }
            }),
        ));

        for (id, character, target_line, target_start, target_end) in
            [(2, 28, 1, 6, 15), (3, 44, 2, 6, 11)]
        {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 5, "character": character}
                }),
            ));
            assert_eq!(
                response.messages[0]["result"],
                json!([{
                    "uri": uri,
                    "range": {
                        "start": {"line": target_line, "character": target_start},
                        "end": {"line": target_line, "character": target_end}
                    }
                }])
            );
        }
    }

    #[test]
    fn definition_resolves_the_checker_selected_super_method() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///SuperCall.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "open class Base {\n\
                             \u{20}\u{20}open fun pick(value: Int): Int = value\n\
                             \u{20}\u{20}open fun pick(value: String): Int = value.length\n\
                             }\n\
                             class Child : Base() {\n\
                             \u{20}\u{20}override fun pick(value: Int): Int = value + 1\n\
                             \u{20}\u{20}fun parent(): Int = super.pick(1)\n\
                             }\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 6, "character": 29}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 1, "character": 11},
                    "end": {"line": 1, "character": 15}
                }
            }])
        );
    }

    #[test]
    fn definition_resolves_an_inherited_super_overload_past_a_namesake() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///InheritedSuperCall.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "open class Grand {\n\
                             \u{20}\u{20}open fun pick(value: Int): Int = value\n\
                             }\n\
                             open class Base : Grand() {\n\
                             \u{20}\u{20}open fun pick(value: String): Int = value.length\n\
                             }\n\
                             class Child : Base() {\n\
                             \u{20}\u{20}fun parent(): Int = super.pick(1)\n\
                             }\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 7, "character": 29}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 1, "character": 11},
                    "end": {"line": 1, "character": 15}
                }
            }])
        );
    }

    #[test]
    fn definition_resolves_an_unqualified_body_property() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///BodyProperty.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class Body {\n\
                             \u{20}\u{20}val value: Int = 1\n\
                             \u{20}\u{20}fun get(): Int = value\n\
                             }\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 2, "character": 20}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 1, "character": 6},
                    "end": {"line": 1, "character": 11}
                }
            }])
        );
    }

    #[test]
    fn definition_includes_backticks_and_resolves_from_the_opening_delimiter() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///Backticked.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun `odd name`(): Int = 1\n\
                             fun use(): Int = `odd name`()\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 17}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 0, "character": 4},
                    "end": {"line": 0, "character": 14}
                }
            }])
        );
    }

    #[test]
    fn definition_includes_backticks_for_constructor_properties_and_enum_entries() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///BacktickedMembers.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class Weird(val `odd name`: Int)\n\
                             fun use(w: Weird): Int = w.`odd name`\n\
                             enum class WeirdEnum { `odd entry` }\n\
                             fun enumUse(): WeirdEnum = WeirdEnum.`odd entry`\n"
                }
            }),
        ));

        for (id, line, character, target_line, target_start, target_end) in
            [(2, 1, 27, 0, 16, 26), (3, 3, 37, 2, 23, 34)]
        {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": line, "character": character}
                }),
            ));
            assert_eq!(
                response.messages[0]["result"],
                json!([{
                    "uri": uri,
                    "range": {
                        "start": {"line": target_line, "character": target_start},
                        "end": {"line": target_line, "character": target_end}
                    }
                }])
            );
        }
    }

    #[test]
    fn definition_on_a_declaration_returns_its_own_range() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///Declaration.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun answer(): Int = 42\n"
                }
            }),
        ));
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 0, "character": 5}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 0, "character": 4},
                    "end": {"line": 0, "character": 10}
                }
            }])
        );
    }

    #[test]
    fn definition_on_an_import_terminal_resolves_the_imported_class() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            ("file:///Imported.kt", "package a\nclass Imported\n"),
            (
                "file:///ImportUse.kt",
                "package b\nimport a.Imported\nfun use(x: Imported): Imported = x\n",
            ),
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
        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///ImportUse.kt"},
                "position": {"line": 1, "character": 10}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": "file:///Imported.kt",
                "range": {
                    "start": {"line": 1, "character": 6},
                    "end": {"line": 1, "character": 14}
                }
            }])
        );
    }

    #[test]
    fn definition_keeps_same_named_classes_package_qualified() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            (
                "file:///A.kt",
                "package a\ndata class Item(val left: Int)\n",
            ),
            (
                "file:///B.kt",
                "package b\ndata class Item(val right: Int)\n",
            ),
            (
                "file:///Use.kt",
                "package b\nfun use(item: Item): Int = item.right\n",
            ),
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

        for (id, character, target_start, target_end) in [(2, 15, 11, 15), (3, 33, 20, 25)] {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": "file:///Use.kt"},
                    "position": {"line": 1, "character": character}
                }),
            ));
            assert_eq!(
                response.messages[0]["result"],
                json!([{
                    "uri": "file:///B.kt",
                    "range": {
                        "start": {"line": 1, "character": target_start},
                        "end": {"line": 1, "character": target_end}
                    }
                }])
            );
        }
    }

    #[test]
    fn definition_does_not_leak_an_unimported_class_across_packages() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            ("file:///Hidden.kt", "package hidden\nclass Secret\n"),
            (
                "file:///Use.kt",
                "package use\nfun unresolved(value: Secret): Secret = value\n",
            ),
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

        for (id, character) in [(2, 22), (3, 31)] {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": "file:///Use.kt"},
                    "position": {"line": 1, "character": character}
                }),
            ));
            assert_eq!(response.messages[0]["result"], json!([]));
        }
    }

    #[test]
    fn definition_resolves_a_class_from_a_wildcard_import() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            ("file:///Imported.kt", "package imported\nclass Visible\n"),
            (
                "file:///Use.kt",
                "package use\nimport imported.*\nfun use(value: Visible): Visible = value\n",
            ),
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

        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///Use.kt"},
                "position": {"line": 2, "character": 15}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": "file:///Imported.kt",
                "range": {
                    "start": {"line": 1, "character": 6},
                    "end": {"line": 1, "character": 13}
                }
            }])
        );
    }

    #[test]
    fn navigation_resolves_nested_types_through_outer_class_imports() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            (
                "file:///Outer.kt",
                "package p\nclass Outer { inner class Inner }\n",
            ),
            (
                "file:///ExplicitUse.kt",
                "package use\nimport p.Outer\nfun f(x: Outer.Inner): Outer.Inner = x\n",
            ),
            (
                "file:///WildcardUse.kt",
                "package use\nimport p.*\nfun f(x: Outer.Inner): Outer.Inner = x\n",
            ),
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

        for (id, uri) in [(2, "file:///ExplicitUse.kt"), (4, "file:///WildcardUse.kt")] {
            let definition = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 2, "character": 16}
                }),
            ));
            assert_eq!(
                definition.messages[0]["result"],
                json!([{
                    "uri": "file:///Outer.kt",
                    "range": {
                        "start": {"line": 1, "character": 26},
                        "end": {"line": 1, "character": 31}
                    }
                }])
            );

            let hover = server.handle(request(
                id + 1,
                "textDocument/hover",
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 2, "character": 16}
                }),
            ));
            assert_eq!(
                hover.messages[0]["result"],
                json!({
                    "contents": {
                        "kind": "markdown",
                        "value": "````kotlin\ninner class Inner\n````\n"
                    },
                    "range": {
                        "start": {"line": 2, "character": 15},
                        "end": {"line": 2, "character": 20}
                    }
                })
            );
        }
    }

    #[test]
    fn navigation_resolves_a_simple_nested_type_inside_its_outer_class() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///Outer.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "package p\nclass Outer { inner class Inner; fun f(x: Inner): Inner = x }\n"
                }
            }),
        ));

        let definition = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 50}
            }),
        ));
        assert_eq!(
            definition.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 1, "character": 26},
                    "end": {"line": 1, "character": 31}
                }
            }])
        );

        let hover = server.handle(request(
            3,
            "textDocument/hover",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 50}
            }),
        ));
        assert_eq!(
            hover.messages[0]["result"],
            json!({
                "contents": {
                    "kind": "markdown",
                    "value": "````kotlin\ninner class Inner\n````\n"
                },
                "range": {
                    "start": {"line": 1, "character": 50},
                    "end": {"line": 1, "character": 55}
                }
            })
        );
    }

    #[test]
    fn definition_does_not_choose_between_ambiguous_wildcard_classes() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            ("file:///A.kt", "package a\nclass Item\n"),
            ("file:///B.kt", "package b\nclass Item\n"),
            (
                "file:///Use.kt",
                "package use\nimport a.*\nimport b.*\nfun use(item: Item): Item = item\n",
            ),
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

        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///Use.kt"},
                "position": {"line": 3, "character": 14}
            }),
        ));
        assert_eq!(response.messages[0]["result"], json!([]));
    }

    #[test]
    fn definition_resolves_an_unambiguous_wildcard_imported_property() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            (
                "file:///Imported.kt",
                "package imported\nval answer: Int = 42\n",
            ),
            (
                "file:///Use.kt",
                "package use\nimport imported.*\nfun use(): Int = answer\n",
            ),
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

        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///Use.kt"},
                "position": {"line": 2, "character": 18}
            }),
        ));
        assert_eq!(
            response.messages[0]["result"],
            json!([{
                "uri": "file:///Imported.kt",
                "range": {
                    "start": {"line": 1, "character": 4},
                    "end": {"line": 1, "character": 10}
                }
            }])
        );
    }

    #[test]
    fn definition_does_not_choose_between_ambiguous_wildcard_properties() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            ("file:///A.kt", "package a\nval answer: Int = 1\n"),
            ("file:///B.kt", "package b\nval answer: Int = 2\n"),
            (
                "file:///Use.kt",
                "package use\nimport a.*\nimport b.*\nfun use(): Int = answer\n",
            ),
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

        let response = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///Use.kt"},
                "position": {"line": 3, "character": 18}
            }),
        ));
        assert_eq!(response.messages[0]["result"], json!([]));
    }

    #[test]
    fn definition_uses_imported_and_qualified_receiver_owners() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        for (uri, text) in [
            (
                "file:///A.kt",
                "package a\ndata class Item(val left: Int)\n",
            ),
            (
                "file:///B.kt",
                "package b\ndata class Item(val right: Int)\n",
            ),
            (
                "file:///Imported.kt",
                "package use\nimport a.Item\nfun read(x: Item): Int = x.left\n",
            ),
            (
                "file:///Qualified.kt",
                "package use\nfun readQualified(x: a.Item): Int = x.left\n",
            ),
            (
                "file:///Local.kt",
                "package use\nfun readLocal(seed: a.Item): Int {\n\
                 \u{20}\u{20}val x: a.Item = seed\n\
                 \u{20}\u{20}return x.left\n\
                 }\n",
            ),
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

        for (id, uri, line, character) in [
            (2, "file:///Imported.kt", 2, 28),
            (3, "file:///Qualified.kt", 1, 39),
            (4, "file:///Local.kt", 3, 12),
        ] {
            let response = server.handle(request(
                id,
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": line, "character": character}
                }),
            ));
            assert_eq!(
                response.messages[0]["result"],
                json!([{
                    "uri": "file:///A.kt",
                    "range": {
                        "start": {"line": 1, "character": 20},
                        "end": {"line": 1, "character": 24}
                    }
                }])
            );
        }
    }

    #[test]
    fn completion_is_scoped_compiler_backed_and_matches_kotlin_item_metadata() {
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
        assert_eq!(completion.messages[0]["result"]["isIncomplete"], true);
        let items = completion.messages[0]["result"]["items"]
            .as_array()
            .unwrap();
        let name = items
            .iter()
            .find(|item| item["label"] == "name")
            .expect("constructor property completion");
        assert_eq!(name["kind"], 6);
        assert_eq!(name["labelDetails"], json!({"description": "String"}));
        assert_eq!(name["sortText"], "0000000000");
        let greeting = items
            .iter()
            .find(|item| item["label"] == "greeting")
            .expect("method completion");
        assert_eq!(greeting["kind"], 2);
        assert_eq!(
            greeting["labelDetails"],
            json!({"detail": "()", "description": "String"})
        );
        assert_eq!(greeting["sortText"], "0000000001");
        assert!(items.iter().all(|item| item["label"] != "later"));

        let resolved = server.handle(request(3, "completionItem/resolve", greeting.clone()));
        assert_eq!(&resolved.messages[0]["result"], greeting);
        assert_eq!(calls.get(), 1, "resolve must not rerun compiler analysis");

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
        assert_eq!(&stale.messages[0]["result"], greeting);
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
    fn unqualified_completion_has_exact_kotlin_metadata_and_ranking() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///CompletionParity.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "package completionparity\n\
                             fun krustyParityTop(): Int = 1\n\
                             fun krustyParityInferred() = 3\n\
                             val krustyParityGlobal: Int = 2\n\
                             fun use(): Int {\n\
                             \u{20}\u{20}val krustyParityLocal: Int = 1\n\
                             \u{20}\u{20}fun krustyParityNested(): Int = 2\n\
                             \u{20}\u{20}return krustyParity\n\
                             }\n"
                }
            }),
        ));

        let completion = server.handle(request(
            2,
            "textDocument/completion",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 7, "character": 21}
            }),
        ));
        assert_eq!(
            completion.messages[0]["result"],
            json!({
                "isIncomplete": true,
                "items": [
                    {
                        "label": "krustyParityLocal",
                        "kind": 6,
                        "labelDetails": {"description": "Int"},
                        "sortText": "0000000000"
                    },
                    {
                        "label": "krustyParityGlobal",
                        "kind": 10,
                        "labelDetails": {
                            "detail": " (completionparity)",
                            "description": "Int"
                        },
                        "sortText": "0000000001"
                    },
                    {
                        "label": "krustyParityNested",
                        "kind": 3,
                        "labelDetails": {"detail": "()", "description": "Int"},
                        "sortText": "0000000002"
                    },
                    {
                        "label": "krustyParityInferred",
                        "kind": 3,
                        "labelDetails": {
                            "detail": "() (completionparity)",
                            "description": "Int"
                        },
                        "sortText": "0000000003"
                    },
                    {
                        "label": "krustyParityTop",
                        "kind": 3,
                        "labelDetails": {
                            "detail": "() (completionparity)",
                            "description": "Int"
                        },
                        "sortText": "0000000004"
                    }
                ]
            })
        );
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
            2
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
    fn incremental_utf16_changes_apply_in_order() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        let initialized = server.handle(request(1, "initialize", json!({})));
        assert_eq!(
            initialized.messages[0]["result"]["capabilities"]["textDocumentSync"],
            2
        );
        let uri = "file:///Incremental.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun before(): Int = 1\nfun use(): Int = before()\n"
                }
            }),
        ));

        server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [
                    {
                        "range": {
                            "start": {"line": 0, "character": 4},
                            "end": {"line": 0, "character": 10}
                        },
                        "rangeLength": 6,
                        "text": "after"
                    },
                    {
                        "range": {
                            "start": {"line": 1, "character": 17},
                            "end": {"line": 1, "character": 23}
                        },
                        "rangeLength": 6,
                        "text": "after"
                    }
                ]
            }),
        ));

        let definition = server.handle(request(
            2,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": 1, "character": 18}
            }),
        ));
        assert_eq!(
            definition.messages[0]["result"],
            json!([{
                "uri": uri,
                "range": {
                    "start": {"line": 0, "character": 4},
                    "end": {"line": 0, "character": 9}
                }
            }])
        );
    }

    #[test]
    fn incremental_changes_use_utf16_and_roll_back_invalid_batches() {
        let analyzed = Rc::new(RefCell::new(Vec::<String>::new()));
        let analyzed_for_server = analyzed.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            analyzed_for_server
                .borrow_mut()
                .extend(sources.iter().map(|source| source.to_string()));
            sources
                .iter()
                .map(|_| super::super::DocumentAnalysis::with_diagnostics(Vec::new()))
                .collect()
        });
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///UnicodeIncremental.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "val text = \"😀x\"\n"
                }
            }),
        ));

        server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{
                    "range": {
                        "start": {"line": 0, "character": 12},
                        "end": {"line": 0, "character": 14}
                    },
                    "rangeLength": 2,
                    "text": "z"
                }]
            }),
        ));
        assert_eq!(analyzed.borrow().last().unwrap(), "val text = \"zx\"\n");
        let analysis_count = analyzed.borrow().len();

        server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 3},
                "contentChanges": [
                    {
                        "range": {
                            "start": {"line": 0, "character": 12},
                            "end": {"line": 0, "character": 13}
                        },
                        "rangeLength": 1,
                        "text": "q"
                    },
                    {
                        "range": {
                            "start": {"line": 99, "character": 0},
                            "end": {"line": 99, "character": 0}
                        },
                        "text": "invalid"
                    }
                ]
            }),
        ));
        assert_eq!(analyzed.borrow().len(), analysis_count);

        server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 3},
                "contentChanges": [{
                    "range": {
                        "start": {"line": 0, "character": 12},
                        "end": {"line": 0, "character": 13}
                    },
                    "rangeLength": 1,
                    "text": "r"
                }]
            }),
        ));
        assert_eq!(analyzed.borrow().last().unwrap(), "val text = \"rx\"\n");
    }

    #[test]
    fn incremental_change_count_is_bounded_before_source_scans() {
        let analyzed = Rc::new(RefCell::new(Vec::<String>::new()));
        let analyzed_for_server = analyzed.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            analyzed_for_server
                .borrow_mut()
                .extend(sources.iter().map(|source| source.to_string()));
            sources
                .iter()
                .map(|_| super::super::DocumentAnalysis::empty())
                .collect()
        });
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///BoundedIncremental.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "x"
                }
            }),
        ));
        let analysis_count = analyzed.borrow().len();
        let too_many = (0..257)
            .map(|_| {
                json!({
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 0}
                    },
                    "text": "a"
                })
            })
            .collect::<Vec<_>>();

        server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": too_many
            }),
        ));
        assert_eq!(analyzed.borrow().len(), analysis_count);

        server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": "accepted"}]
            }),
        ));
        assert_eq!(analyzed.borrow().last().unwrap(), "accepted");
    }

    #[test]
    fn blocked_document_requires_a_full_change_before_incremental_recovery() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_server = calls.clone();
        let short_sources = Rc::new(RefCell::new(Vec::<String>::new()));
        let short_sources_for_server = short_sources.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            calls_for_server.set(calls_for_server.get() + 1);
            short_sources_for_server.borrow_mut().extend(
                sources
                    .iter()
                    .filter(|source| source.len() < 64)
                    .map(|source| source.to_string()),
            );
            sources
                .iter()
                .map(|_| super::super::DocumentAnalysis::empty())
                .collect()
        });
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///BlockedIncremental.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "original"
                }
            }),
        ));
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///Filler.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "x".repeat(crate::worker::MAX_SOURCE_SET_BYTES - 16)
                }
            }),
        ));

        server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": "this replacement exceeds the remaining budget"}]
            }),
        ));
        let analysis_count = calls.get();

        server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 3},
                "contentChanges": [{
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 0}
                    },
                    "text": "corrupt"
                }]
            }),
        ));
        assert_eq!(calls.get(), analysis_count);
        assert!(!short_sources
            .borrow()
            .iter()
            .any(|source| source == "corrupt"));

        server.handle(notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 3},
                "contentChanges": [{"text": "restored"}]
            }),
        ));
        assert_eq!(short_sources.borrow().last().unwrap(), "restored");
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
    fn queued_incremental_changes_apply_in_order_with_one_analysis() {
        let analyzed = Rc::new(RefCell::new(Vec::<String>::new()));
        let analyzed_for_server = analyzed.clone();
        let mut server = LspService::new(move |sources: &[&str]| {
            analyzed_for_server
                .borrow_mut()
                .extend(sources.iter().map(|source| source.to_string()));
            sources
                .iter()
                .map(|_| super::super::DocumentAnalysis::empty())
                .collect()
        });
        server.handle(request(1, "initialize", json!({})));
        let uri = "file:///incremental.kt";
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun before() = 1\nfun use() = before()\n"
                }
            }),
        ));
        assert_eq!(analyzed.borrow().len(), 1);

        let first = notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{
                    "range": {
                        "start": {"line": 0, "character": 4},
                        "end": {"line": 0, "character": 10}
                    },
                    "rangeLength": 6,
                    "text": "after"
                }]
            }),
        );
        let second = notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": 3},
                "contentChanges": [{
                    "range": {
                        "start": {"line": 1, "character": 12},
                        "end": {"line": 1, "character": 18}
                    },
                    "rangeLength": 6,
                    "text": "after"
                }]
            }),
        );
        let (sender, receiver) = std::sync::mpsc::sync_channel(2);
        sender.send(Incoming::Message(second)).unwrap();
        drop(sender);
        let mut pending = std::collections::VecDeque::new();

        let changes = coalesce_document_notifications(first, &receiver, &mut pending);
        assert_eq!(changes.len(), 2);
        let mut output = Vec::new();
        super::implementation::dispatch_document_batch(&mut output, &mut server, changes).unwrap();

        assert_eq!(analyzed.borrow().len(), 2);
        assert_eq!(
            analyzed.borrow().last().unwrap(),
            "fun after() = 1\nfun use() = after()\n"
        );
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
    fn full_change_coalescing_does_not_cross_another_document_notification() {
        let first = notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///a.kt", "version": 2},
                "contentChanges": [{"text": "a2"}]
            }),
        );
        let between = notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///b.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "b1"
                }
            }),
        );
        let latest = notification(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": "file:///a.kt", "version": 3},
                "contentChanges": [{"text": "a3"}]
            }),
        );
        let (sender, receiver) = std::sync::mpsc::sync_channel(3);
        sender.send(Incoming::Message(between)).unwrap();
        sender.send(Incoming::Message(latest)).unwrap();
        drop(sender);
        let mut pending = std::collections::VecDeque::new();

        let changes = coalesce_document_notifications(first, &receiver, &mut pending);
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0]["params"]["textDocument"]["version"], 2);
        assert_eq!(changes[1]["params"]["textDocument"]["uri"], "file:///b.kt");
        assert_eq!(changes[2]["params"]["textDocument"]["version"], 3);
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
            json!({
                "kind": "markdown",
                "value": "````kotlin\nval answer: Int\n````\n"
            })
        );
        assert_eq!(
            hover.messages[0]["result"]["range"],
            json!({
                "start": {"line": 0, "character": 41},
                "end": {"line": 0, "character": 47}
            })
        );
        let literal = server.handle(request(
            3,
            "textDocument/hover",
            json!({
                "textDocument": {"uri": "file:///main.kt"},
                "position": {"line": 0, "character": 30}
            }),
        ));
        assert_eq!(calls.get(), 1, "hover must use the cached symbol index");
        assert_eq!(literal.messages[0]["result"], Value::Null);
    }

    #[test]
    fn hover_range_includes_backticks_for_parameter_references() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///main.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun f(`odd param`: Int): Int = `odd param`"
                }
            }),
        ));

        let hover = server.handle(request(
            2,
            "textDocument/hover",
            json!({
                "textDocument": {"uri": "file:///main.kt"},
                "position": {"line": 0, "character": 36}
            }),
        ));
        assert_eq!(
            hover.messages[0]["result"],
            json!({
                "contents": {
                    "kind": "markdown",
                    "value": "````kotlin\n`odd param`: Int\n````\n"
                },
                "range": {
                    "start": {"line": 0, "character": 31},
                    "end": {"line": 0, "character": 42}
                }
            })
        );
    }

    #[test]
    fn hover_prefers_a_selected_member_over_a_same_named_inner_class() {
        let mut server = LspService::new(super::super::analyze_for_lsp);
        server.handle(request(1, "initialize", json!({})));
        server.handle(notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///main.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "class Outer { inner class Item; fun Item(): Int = 1 }\nfun use(outer: Outer): Int = outer.Item()"
                }
            }),
        ));

        let hover = server.handle(request(
            2,
            "textDocument/hover",
            json!({
                "textDocument": {"uri": "file:///main.kt"},
                "position": {"line": 1, "character": 36}
            }),
        ));
        assert_eq!(
            hover.messages[0]["result"],
            json!({
                "contents": {
                    "kind": "markdown",
                    "value": "````kotlin\ninner class Item\n````\n\n---\n````kotlin\nfun Item(): Int\n````\n"
                },
                "range": {
                    "start": {"line": 1, "character": 35},
                    "end": {"line": 1, "character": 39}
                }
            })
        );

        let definition = server.handle(request(
            3,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": "file:///main.kt"},
                "position": {"line": 1, "character": 36}
            }),
        ));
        assert_eq!(
            definition.messages[0]["result"],
            json!([
                {
                    "uri": "file:///main.kt",
                    "range": {
                        "start": {"line": 0, "character": 26},
                        "end": {"line": 0, "character": 30}
                    }
                },
                {
                    "uri": "file:///main.kt",
                    "range": {
                        "start": {"line": 0, "character": 36},
                        "end": {"line": 0, "character": 40}
                    }
                }
            ])
        );
    }
}
