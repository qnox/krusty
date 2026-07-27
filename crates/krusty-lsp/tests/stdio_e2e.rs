mod common;

use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use krusty_lsp::{read_framed, write_framed, MAX_MESSAGE_BYTES};
use serde_json::{json, Value};

use common::TempProject;

struct ServerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl ServerProcess {
    fn start(arguments: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_krusty-lsp"));
        command.args(["--stdio", "-no-jdk"]).args(arguments);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start krusty-lsp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, message: &Value) {
        write_framed(&mut self.stdin, &serde_json::to_vec(message).unwrap()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn receive(&mut self) -> Option<Value> {
        read_framed(&mut self.stdout, MAX_MESSAGE_BYTES)
            .unwrap()
            .map(|body| serde_json::from_slice(&body).unwrap())
    }

    fn receive_until(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        loop {
            let message = self.receive().expect("LSP response");
            if predicate(&message) {
                return message;
            }
        }
    }

    fn finish(self) -> Vec<Value> {
        let Self {
            mut child,
            stdin,
            mut stdout,
        } = self;
        drop(stdin);
        let mut output = Vec::new();
        while let Some(body) = read_framed(&mut stdout, MAX_MESSAGE_BYTES).unwrap() {
            output.push(serde_json::from_slice(&body).unwrap());
        }
        assert!(child.wait().unwrap().success());
        output
    }
}

fn run_server(arguments: &[&str], messages: &[Value]) -> Vec<Value> {
    let mut server = ServerProcess::start(arguments);
    for message in messages {
        server.send(message);
    }
    server.finish()
}

#[test]
fn stdio_server_uses_the_compiler_worker_and_exits_cleanly() {
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///main.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun answer(): Int = 42\n\
                             fun box(): Int = \"no\"\n\
                             fun use(): Int = ans\n\
                             fun navigate(): Int = answer()"
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": "file:///main.kt"},
                "position": {"line": 2, "character": 20}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/definition",
            "params": {
                "textDocument": {"uri": "file:///main.kt"},
                "position": {"line": 3, "character": 23}
            }
        }),
        json!({"jsonrpc": "2.0", "id": 4, "method": "shutdown", "params": null}),
        json!({"jsonrpc": "2.0", "method": "exit", "params": null}),
    ];
    let output = run_server(&[], &messages);
    assert_eq!(output[0]["id"], 1);
    assert_eq!(output[1]["method"], "textDocument/publishDiagnostics");
    assert_eq!(
        output[1]["params"]["diagnostics"][0]["message"],
        "Return type mismatch: expected 'Int', actual 'String'."
    );
    assert_eq!(output[2]["id"], 2);
    assert!(output[2]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["label"] == "answer" && item["kind"] == 3));
    assert_eq!(output[3]["id"], 3);
    assert_eq!(
        output[3]["result"],
        json!([{
            "uri": "file:///main.kt",
            "range": {
                "start": {"line": 0, "character": 4},
                "end": {"line": 0, "character": 10}
            }
        }])
    );
    assert_eq!(output[4]["id"], 4);
}

#[test]
fn stdio_server_reports_official_unknown_named_argument_diagnostic() {
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///named.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun pair(left: Int, right: String): String = right\n\
                             fun use(): String = pair(left = 1, unknown = 2, right = \"ok\")"
                }
            }
        }),
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": null}),
        json!({"jsonrpc": "2.0", "method": "exit", "params": null}),
    ];
    let output = run_server(&[], &messages);
    let diagnostics = output
        .iter()
        .find(|message| message["method"] == "textDocument/publishDiagnostics")
        .and_then(|message| message["params"]["diagnostics"].as_array())
        .expect("published diagnostics");
    assert_eq!(
        diagnostics,
        &[json!({
            "range": {
                "start": {"line": 1, "character": 35},
                "end": {"line": 1, "character": 42}
            },
            "severity": 1,
            "source": "Kotlin",
            "message": "No parameter with name 'unknown' found."
        })]
    );
}

#[test]
fn stdio_server_reports_official_duplicate_named_argument_diagnostic() {
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///duplicate.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun pair(a: Int, b: String): String = b\n\
                             fun use(): String = pair(a = 1, a = 2, b = \"ok\")"
                }
            }
        }),
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": null}),
        json!({"jsonrpc": "2.0", "method": "exit", "params": null}),
    ];
    let output = run_server(&[], &messages);
    let diagnostics = output
        .iter()
        .find(|message| message["method"] == "textDocument/publishDiagnostics")
        .and_then(|message| message["params"]["diagnostics"].as_array())
        .expect("published diagnostics");
    assert_eq!(
        diagnostics,
        &[json!({
            "range": {
                "start": {"line": 1, "character": 32},
                "end": {"line": 1, "character": 33}
            },
            "severity": 1,
            "source": "Kotlin",
            "message": "Argument already passed for this parameter."
        })]
    );
}

#[test]
fn stdio_server_reports_bare_return_type_mismatch() {
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///return.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun value(): Int { return }"
                }
            }
        }),
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": null}),
        json!({"jsonrpc": "2.0", "method": "exit", "params": null}),
    ];
    let output = run_server(&[], &messages);
    let diagnostics = output
        .iter()
        .find(|message| message["method"] == "textDocument/publishDiagnostics")
        .and_then(|message| message["params"]["diagnostics"].as_array())
        .expect("published diagnostics");
    assert_eq!(
        diagnostics,
        &[json!({
            "range": {
                "start": {"line": 0, "character": 19},
                "end": {"line": 0, "character": 25}
            },
            "severity": 1,
            "source": "Kotlin",
            "message": "Return type mismatch: expected 'Int', actual 'Unit'."
        })]
    );
}

#[test]
fn stdio_server_applies_configured_language_features() {
    let source = "\
data class Entry(val first: String, val second: String)
fun combine(entries: Array<Entry>): String {
    var result = \"\"
    for ([left, right] in entries) {
        result += left + right
    }
    return result
}";
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///feature.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": source
                }
            }
        }),
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": null}),
        json!({"jsonrpc": "2.0", "method": "exit", "params": null}),
    ];
    let output = run_server(&["-Xname-based-destructuring=complete"], &messages);
    let diagnostics = output
        .iter()
        .find(|message| message["method"] == "textDocument/publishDiagnostics")
        .and_then(|message| message["params"]["diagnostics"].as_array())
        .expect("published diagnostics");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn stdio_server_indexes_unopened_project_sources() {
    let project = TempProject::new("unopened-source");
    project.write(
        "src/Model.kt",
        "package sample\ndata class Item(val label: String, val rank: Int?)\n",
    );
    let use_uri = project.uri("src/Use.kt");
    let root_uri: String = url::Url::from_directory_path(project.path())
        .expect("temporary project root is a file URI")
        .into();
    let messages = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"rootUri": root_uri}
        }),
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": use_uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "package sample\nfun make() = Item(label = \"ok\", rank = null)\n"
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/definition",
            "params": {
                "textDocument": {"uri": use_uri},
                "position": {"line": 1, "character": 14}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/rename",
            "params": {
                "textDocument": {"uri": use_uri},
                "position": {"line": 1, "character": 14},
                "newName": "Renamed"
            }
        }),
        json!({"jsonrpc": "2.0", "id": 4, "method": "shutdown", "params": null}),
        json!({"jsonrpc": "2.0", "method": "exit", "params": null}),
    ];

    let output = run_server(&[], &messages);
    let watchers = output
        .iter()
        .find(|message| message["method"] == "client/registerCapability")
        .and_then(|message| {
            message["params"]["registrations"][0]["registerOptions"]["watchers"].as_array()
        })
        .expect("registered file watchers");
    assert!(watchers
        .iter()
        .any(|watcher| watcher["globPattern"] == "**/*.kt"));
    let diagnostics = output
        .iter()
        .find(|message| {
            message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == use_uri
        })
        .and_then(|message| message["params"]["diagnostics"].as_array())
        .expect("published use-site diagnostics");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let definition = output
        .iter()
        .find(|message| message["id"] == 2)
        .expect("definition response");
    assert_eq!(definition["result"][0]["uri"], project.uri("src/Model.kt"));
    let rename = output
        .iter()
        .find(|message| message["id"] == 3)
        .expect("rename response");
    assert_eq!(rename["result"], Value::Null);
}

#[test]
fn stdio_server_reanalyzes_after_an_unopened_source_changes() {
    let project = TempProject::new("changed-unopened-source");
    project.write("src/Model.kt", "package sample\nclass Item\n");
    let use_uri = project.uri("src/Use.kt");
    let root_uri: String = url::Url::from_directory_path(project.path())
        .expect("temporary project root is a file URI")
        .into();
    let mut server = ServerProcess::start(&[]);
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"rootUri": root_uri}
    }));
    server.receive_until(|message| message["id"] == 1);
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": use_uri,
                "languageId": "kotlin",
                "version": 1,
                "text": "package sample\nfun make() = Item()\n"
            }
        }
    }));
    let initial = server.receive_until(|message| {
        message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == use_uri
    });
    assert_eq!(initial["params"]["diagnostics"], json!([]));

    project.write("src/Model.kt", "package sample\nclass Replacement\n");
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "workspace/didChangeWatchedFiles",
        "params": {
            "changes": [{"uri": project.uri("src/Model.kt"), "type": 2}]
        }
    }));
    let changed = server.receive_until(|message| {
        message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == use_uri
    });
    let diagnostics = changed["params"]["diagnostics"].as_array().unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["message"] == "Unresolved function 'Item'"),
        "{diagnostics:?}"
    );

    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "shutdown",
        "params": null
    }));
    server.send(&json!({"jsonrpc": "2.0", "method": "exit", "params": null}));
    let _ = server.finish();
}

#[test]
fn stdio_server_suppresses_semantic_diagnostics_for_an_incomplete_source_set() {
    let project = TempProject::new("oversized-source-set");
    let oversized = project.path().join("src/Oversized.kt");
    std::fs::create_dir_all(oversized.parent().unwrap()).unwrap();
    let file = std::fs::File::create(oversized).unwrap();
    file.set_len(krusty_lsp::MAX_SOURCE_SET_BYTES as u64 + 1)
        .unwrap();
    let use_uri = project.uri("src/Use.kt");
    let root_uri: String = url::Url::from_directory_path(project.path())
        .expect("temporary project root is a file URI")
        .into();
    let messages = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"rootUri": root_uri}
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": use_uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun use() = missing()\n"
                }
            }
        }),
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": null}),
        json!({"jsonrpc": "2.0", "method": "exit", "params": null}),
    ];

    let output = run_server(&[], &messages);
    let diagnostics = output
        .iter()
        .find(|message| {
            message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == use_uri
        })
        .and_then(|message| message["params"]["diagnostics"].as_array())
        .expect("published source-limit diagnostic");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(diagnostics[0]["message"]
        .as_str()
        .is_some_and(|message| message.contains("semantic diagnostics suppressed")));
}
