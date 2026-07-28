mod common;

use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use krusty_lsp::{read_framed, write_framed, MAX_MESSAGE_BYTES};
use serde_json::{json, Value};

use common::TempProject;

const PIPE_TIMEOUT: Duration = Duration::from_secs(45);

const EVENT_QUEUE_CAPACITY: usize = 64;

enum PipeEvent {
    Message(Vec<u8>),
    Closed,
}

struct ServerProcess {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<PipeEvent>,
    timeout: Duration,
}

impl ServerProcess {
    fn start(arguments: &[&str]) -> Self {
        Self::start_with_timeout(arguments, PIPE_TIMEOUT)
    }

    fn start_with_timeout(arguments: &[&str], timeout: Duration) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_krusty-lsp"));
        command.args(["--stdio", "-no-jdk"]).args(arguments);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start krusty-lsp");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());

        let (sender, events) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        thread::spawn(move || loop {
            match read_framed(&mut stdout, MAX_MESSAGE_BYTES) {
                Ok(Some(body)) => {
                    if sender.send(PipeEvent::Message(body)).is_err() {
                        break;
                    }
                }
                Ok(None) | Err(_) => {
                    let _ = sender.send(PipeEvent::Closed);
                    break;
                }
            }
        });

        Self {
            child,
            stdin,
            events,
            timeout,
        }
    }

    fn send(&mut self, message: &Value) {
        write_framed(&mut self.stdin, &serde_json::to_vec(message).unwrap()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn receive(&mut self) -> Option<Value> {
        match self.events.recv_timeout(self.timeout) {
            Ok(PipeEvent::Message(body)) => Some(serde_json::from_slice(&body).unwrap()),
            Ok(PipeEvent::Closed) | Err(RecvTimeoutError::Disconnected) => None,
            Err(RecvTimeoutError::Timeout) => {
                let timeout = self.timeout;
                panic!(
                    "no LSP message received within {timeout:?}; the server appears to be alive \
                     but silent"
                )
            }
        }
    }

    fn receive_until(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        loop {
            let message = self.receive().expect("LSP response");
            if predicate(&message) {
                return message;
            }
        }
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.receive_until(|message| message["id"] == id)
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn await_diagnostics(&mut self, uri: &str) -> Vec<Value> {
        let message = self.receive_until(|message| {
            message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == uri
        });
        message["params"]["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    fn shutdown_and_exit(mut self) {
        let _ = self.request(i64::MAX, "shutdown", Value::Null);
        self.notify("exit", Value::Null);
        let _ = self.finish();
    }

    fn finish(self) -> Vec<Value> {
        let Self {
            mut child,
            stdin,
            events,
            timeout,
        } = self;
        drop(stdin);
        let mut output = Vec::new();
        loop {
            match events.recv_timeout(timeout) {
                Ok(PipeEvent::Message(body)) => output.push(serde_json::from_slice(&body).unwrap()),
                Ok(PipeEvent::Closed) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!(
                    "the server did not close its output within {timeout:?} after stdin closed"
                ),
            }
        }
        assert!(wait_with_timeout(&mut child, timeout).success());
        output
    }
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            panic!("server process did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[should_panic(expected = "the server appears to be alive but silent")]
fn receive_until_panics_instead_of_hanging_when_the_server_stays_silent() {
    let mut server = ServerProcess::start_with_timeout(&[], Duration::from_secs(2));
    server.request(1, "initialize", json!({}));
    server.receive_until(|message| message["method"] == "this/notification/never/arrives");
}

fn diagnostics_after_open(arguments: &[&str], uri: &str, text: &str) -> Vec<Value> {
    let mut server = ServerProcess::start(arguments);
    server.request(1, "initialize", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kotlin",
                "version": 1,
                "text": text
            }
        }),
    );
    let diagnostics = server.await_diagnostics(uri);
    server.shutdown_and_exit();
    diagnostics
}

#[test]
fn stdio_server_uses_the_compiler_worker_and_exits_cleanly() {
    let mut server = ServerProcess::start(&[]);
    let initialize = server.request(1, "initialize", json!({}));
    assert_eq!(initialize["id"], 1);
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": "file:///main.kt",
                "languageId": "kotlin",
                "version": 1,
                "text": "fun answer(): Int = 42\n\
                         fun box(): Int = \"no\"\n\
                         fun use(): Int = ans\n\
                         fun navigate(): Int = answer()"
            }
        }),
    );

    let diagnostics = server.await_diagnostics("file:///main.kt");
    assert_eq!(
        diagnostics[0]["message"],
        "Return type mismatch: expected 'Int', actual 'String'."
    );

    // Requests are answered from the analyzed snapshot once diagnostics have landed.
    let completion = server.request(
        2,
        "textDocument/completion",
        json!({
            "textDocument": {"uri": "file:///main.kt"},
            "position": {"line": 2, "character": 20}
        }),
    );
    assert!(completion["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["label"] == "answer" && item["kind"] == 3));

    let definition = server.request(
        3,
        "textDocument/definition",
        json!({
            "textDocument": {"uri": "file:///main.kt"},
            "position": {"line": 3, "character": 23}
        }),
    );
    assert_eq!(
        definition["result"],
        json!([{
            "uri": "file:///main.kt",
            "range": {
                "start": {"line": 0, "character": 4},
                "end": {"line": 0, "character": 10}
            }
        }])
    );

    server.shutdown_and_exit();
}

#[test]
fn stdio_server_reports_official_unknown_named_argument_diagnostic() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///named.kt",
        "fun pair(left: Int, right: String): String = right\n\
         fun use(): String = pair(left = 1, unknown = 2, right = \"ok\")",
    );
    assert_eq!(
        diagnostics,
        vec![json!({
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
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///duplicate.kt",
        "fun pair(a: Int, b: String): String = b\n\
         fun use(): String = pair(a = 1, a = 2, b = \"ok\")",
    );
    assert_eq!(
        diagnostics,
        vec![json!({
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
fn stdio_server_reports_mixed_and_missing_argument_diagnostics() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///mixed.kt",
        "fun combine(first: Int, second: Int): Int = first + second\n\
         fun use(): Int = combine(second = 2, 1)",
    );
    let messages: Vec<_> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic["message"].as_str().unwrap())
        .collect();

    assert_eq!(
        messages,
        [
            "Mixing named and positional arguments is not allowed unless the order of the arguments matches the order of the parameters.",
            "No value passed for parameter 'first'.",
        ]
    );
    assert_eq!(
        diagnostics[0]["range"],
        json!({
            "start": {"line": 1, "character": 37},
            "end": {"line": 1, "character": 38}
        })
    );
    assert_eq!(
        diagnostics[1]["range"],
        json!({
            "start": {"line": 1, "character": 17},
            "end": {"line": 1, "character": 24}
        })
    );
}

#[test]
fn stdio_server_reports_bare_return_type_mismatch() {
    let diagnostics =
        diagnostics_after_open(&[], "file:///return.kt", "fun value(): Int { return }");
    assert_eq!(
        diagnostics,
        vec![json!({
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
fn stdio_server_reports_incompatible_equality_diagnostics() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///equality.kt",
        "fun equal(): Boolean = 1 == \"text\"\n\
         fun unequal(): Boolean = 1 != \"text\"",
    );
    assert_eq!(
        diagnostics,
        vec![
            json!({
                "range": {
                    "start": {"line": 0, "character": 23},
                    "end": {"line": 0, "character": 34}
                },
                "severity": 2,
                "source": null,
                "message": "Boolean expression can be simplified"
            }),
            json!({
                "range": {
                    "start": {"line": 0, "character": 23},
                    "end": {"line": 0, "character": 34}
                },
                "severity": 1,
                "source": "Kotlin",
                "message": "Operator '==' cannot be applied to 'Int' and 'String'."
            }),
            json!({
                "range": {
                    "start": {"line": 1, "character": 25},
                    "end": {"line": 1, "character": 36}
                },
                "severity": 2,
                "source": null,
                "message": "Boolean expression can be simplified"
            }),
            json!({
                "range": {
                    "start": {"line": 1, "character": 25},
                    "end": {"line": 1, "character": 36}
                },
                "severity": 1,
                "source": "Kotlin",
                "message": "Operator '!=' cannot be applied to 'Int' and 'String'."
            }),
        ]
    );
}

#[test]
fn stdio_server_reports_non_exhaustive_when_keyword_range() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///when.kt",
        "enum class Phase { FIRST, SECOND }\n\
         fun value(phase: Phase): Int = when (phase) { Phase.FIRST -> 1 }",
    );
    assert_eq!(
        diagnostics,
        vec![json!({
            "range": {
                "start": {"line": 1, "character": 31},
                "end": {"line": 1, "character": 35}
            },
            "severity": 1,
            "source": "Kotlin",
            "message": "'when' expression must be exhaustive. Add the 'SECOND' branch or an 'else' branch."
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
    let diagnostics = diagnostics_after_open(
        &["-Xname-based-destructuring=complete"],
        "file:///feature.kt",
        source,
    );
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

    let mut server = ServerProcess::start(&[]);
    server.request(1, "initialize", json!({"rootUri": root_uri}));
    server.notify("initialized", json!({}));

    // The watcher registration arrives asynchronously once the engine has loaded the project.
    let registration =
        server.receive_until(|message| message["method"] == "client/registerCapability");
    let watchers = registration["params"]["registrations"][0]["registerOptions"]["watchers"]
        .as_array()
        .expect("registered file watchers");
    assert!(watchers
        .iter()
        .any(|watcher| watcher["globPattern"] == "**/*.kt"));

    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": use_uri,
                "languageId": "kotlin",
                "version": 1,
                "text": "package sample\nfun make() = Item(label = \"ok\", rank = null)\n"
            }
        }),
    );
    let diagnostics = server.await_diagnostics(&use_uri);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let definition = server.request(
        2,
        "textDocument/definition",
        json!({
            "textDocument": {"uri": use_uri},
            "position": {"line": 1, "character": 14}
        }),
    );
    assert_eq!(definition["result"][0]["uri"], project.uri("src/Model.kt"));

    let rename = server.request(
        3,
        "textDocument/rename",
        json!({
            "textDocument": {"uri": use_uri},
            "position": {"line": 1, "character": 14},
            "newName": "Renamed"
        }),
    );
    assert_eq!(rename["result"], Value::Null);

    server.shutdown_and_exit();
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
    server.request(1, "initialize", json!({"rootUri": root_uri}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": use_uri,
                "languageId": "kotlin",
                "version": 1,
                "text": "package sample\nfun make() = Item()\n"
            }
        }),
    );
    let initial = server.await_diagnostics(&use_uri);
    assert_eq!(initial, Vec::<Value>::new());

    project.write("src/Model.kt", "package sample\nclass Replacement\n");
    server.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{"uri": project.uri("src/Model.kt"), "type": 2}] }),
    );
    // The reanalysis publishes fresh diagnostics for the open document; wait for the one that
    // reflects the changed dependency (a bare acknowledgement publish may precede it).
    let changed = server.receive_until(|message| {
        message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == use_uri
            && message["params"]["diagnostics"]
                .as_array()
                .is_some_and(|diagnostics| {
                    diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic["message"] == "Unresolved function 'Item'")
                })
    });
    assert!(changed["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|diagnostic| diagnostic["message"] == "Unresolved function 'Item'"));

    server.shutdown_and_exit();
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

    let mut server = ServerProcess::start(&[]);
    server.request(1, "initialize", json!({"rootUri": root_uri}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": use_uri,
                "languageId": "kotlin",
                "version": 1,
                "text": "fun use() = missing()\n"
            }
        }),
    );
    let diagnostics = server.receive_until(|message| {
        message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == use_uri
            && message["params"]["diagnostics"]
                .as_array()
                .is_some_and(|diagnostics| !diagnostics.is_empty())
    });
    let diagnostics = diagnostics["params"]["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(diagnostics[0]["message"]
        .as_str()
        .is_some_and(|message| message.contains("semantic diagnostics suppressed")));
    server.shutdown_and_exit();
}
