mod common;

use std::io::{BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use krusty::jvm::classfile::ClassWriter;
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

    fn start_in(arguments: &[&str], current_dir: &Path) -> Self {
        Self::start_with_timeout_in(arguments, PIPE_TIMEOUT, Some(current_dir))
    }

    fn start_with_timeout(arguments: &[&str], timeout: Duration) -> Self {
        Self::start_with_timeout_in(arguments, timeout, None)
    }

    fn start_with_timeout_in(
        arguments: &[&str],
        timeout: Duration,
        current_dir: Option<&Path>,
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_krusty-lsp"));
        command.args(["--stdio", "-no-jdk"]).args(arguments);
        if let Some(current_dir) = current_dir {
            command.current_dir(current_dir);
        }
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

    fn await_diagnostics_for(
        &mut self,
        uris: &[&str],
    ) -> std::collections::HashMap<String, Vec<Value>> {
        let mut diagnostics = std::collections::HashMap::new();
        while diagnostics.len() < uris.len() {
            let message = self.receive().expect("LSP response");
            if message["method"] != "textDocument/publishDiagnostics" {
                continue;
            }
            let Some(uri) = message["params"]["uri"].as_str() else {
                continue;
            };
            if !uris.contains(&uri) {
                continue;
            }
            diagnostics.insert(
                uri.to_string(),
                message["params"]["diagnostics"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        diagnostics
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
fn a_document_outside_every_module_is_still_analysed() {
    let project = TempProject::new("unowned-open-document");
    project.write(
        ".idea/modules.xml",
        r#"<project version="4">
             <component name="ProjectModuleManager">
               <modules>
                 <module fileurl="file://$PROJECT_DIR$/core/core.iml" filepath="$PROJECT_DIR$/core/core.iml" />
               </modules>
             </component>
           </project>"#,
    );
    project.write(
        "core/core.iml",
        r#"<module>
             <component name="NewModuleRootManager">
               <content url="file://$MODULE_DIR$">
                 <sourceFolder url="file://$MODULE_DIR$/src/main/kotlin" isTestSource="false" />
               </content>
             </component>
           </module>"#,
    );

    // The document sits outside every module source root. It still belongs to no module after the
    // model loads, which must not cost it its analysis.
    let uri = "file:///scratch.kt";
    let mut server = ServerProcess::start_in(&[], project.path());
    server.request(1, "initialize", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kotlin",
                "version": 1,
                "text": "fun returnMismatch(): String = 1"
            }
        }),
    );

    let diagnostics = server.await_diagnostics(uri);
    assert_eq!(
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic["message"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["Return type mismatch: expected 'String', actual 'Int'."]
    );
    server.shutdown_and_exit();
}

#[test]
fn two_documents_outside_every_module_do_not_see_each_other() {
    // Both documents sit outside every module source root of the loaded model, so each is analysed
    // as its OWN group. Pooling them would make two unrelated scratch files one source set, and the
    // same top-level declaration in both would then be a conflict that neither file has on its own.
    // (A workspace with NO model is a different case: there every open document is one source set
    // by design, since a plain folder of `.kt` files is meant to see itself.)
    let project = TempProject::new("two-unowned-open-documents");
    project.write(
        ".idea/modules.xml",
        r#"<project version="4">
             <component name="ProjectModuleManager">
               <modules>
                 <module fileurl="file://$PROJECT_DIR$/core/core.iml" filepath="$PROJECT_DIR$/core/core.iml" />
               </modules>
             </component>
           </project>"#,
    );
    project.write(
        "core/core.iml",
        r#"<module>
             <component name="NewModuleRootManager">
               <content url="file://$MODULE_DIR$">
                 <sourceFolder url="file://$MODULE_DIR$/src/main/kotlin" isTestSource="false" />
               </content>
             </component>
           </module>"#,
    );

    let first = "file:///scratch-one.kt";
    let second = "file:///scratch-two.kt";
    let mut server = ServerProcess::start_in(&[], project.path());
    server.request(1, "initialize", json!({}));
    for uri in [first, second] {
        server.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun shared(): String = \"scratch\"\nfun unusedReturn(): String = 1\n"
                }
            }),
        );
    }

    for uri in [first, second] {
        let diagnostics = server.await_diagnostics(uri);
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic["message"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            ["Return type mismatch: expected 'String', actual 'Int'."],
            "{uri} must report only its own diagnostic, not a conflict with the other scratch file"
        );
    }
    server.shutdown_and_exit();
}

#[test]
fn pull_diagnostics_wait_for_the_current_open_document_analysis() {
    let uri = "file:///return.kt";
    let mut server = ServerProcess::start(&[]);
    server.request(1, "initialize", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kotlin",
                "version": 1,
                "text": "fun returnMismatch(): String = 1"
            }
        }),
    );

    let response = server.request(
        2,
        "textDocument/diagnostic",
        json!({"textDocument": {"uri": uri}}),
    );
    assert_eq!(response["result"]["kind"], "full");
    assert!(response["result"]["resultId"].is_string());
    assert_eq!(
        response["result"]["items"],
        json!([{
            "range": {
                "start": {"line": 0, "character": 31},
                "end": {"line": 0, "character": 32}
            },
            "severity": 1,
            "source": "Kotlin",
            "message": "Return type mismatch: expected 'String', actual 'Int'."
        }])
    );
    server.shutdown_and_exit();
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
fn stdio_server_starts_a_worker_for_an_oversized_jps_classpath() {
    const ENTRY_COUNT: usize = 2_752;
    let project = TempProject::new("oversized-worker-classpath");
    project.write(
        ".idea/modules.xml",
        r#"<project><component name="ProjectModuleManager"><modules>
             <module filepath="$PROJECT_DIR$/app/app.iml" />
           </modules></component></project>"#,
    );
    project.write(
        "app/app.iml",
        r#"<module><component name="NewModuleRootManager">
             <content url="file://$MODULE_DIR$">
               <sourceFolder url="file://$MODULE_DIR$/src" isTestSource="false" />
             </content>
             <orderEntry type="library" name="oversized" level="project" />
           </component></module>"#,
    );
    let mut library =
        String::from(r#"<component name="libraryTable"><library name="oversized"><CLASSES>"#);
    let entry_suffix = "segment".repeat(10);
    for index in 0..ENTRY_COUNT {
        library.push_str(&format!(
            r#"<root url="file://$PROJECT_DIR$/dependencies/component-{index:04}-{}" />"#,
            entry_suffix
        ));
    }
    library.push_str("</CLASSES></library></component>");
    assert!(
        library.len() > 128 * 1024,
        "the JPS classpath must exceed the common Unix per-argument ceiling"
    );
    project.write(".idea/libraries/oversized.xml", &library);
    let final_entry = project.path().join(format!(
        "dependencies/component-{:04}-{entry_suffix}/oversized",
        ENTRY_COUNT - 1
    ));
    std::fs::create_dir_all(&final_entry).expect("create final classpath entry");
    std::fs::write(
        final_entry.join("LastEntry.class"),
        ClassWriter::new("oversized/LastEntry", "java/lang/Object").finish(),
    )
    .expect("write final classpath class");
    let source = "import oversized.LastEntry\nfun identity(value: LastEntry): LastEntry = value\n";
    let uri = project.uri("app/src/Main.kt");
    project.write("app/src/Main.kt", source);
    let root_uri: String = url::Url::from_directory_path(project.path())
        .expect("temporary project root is a file URI")
        .into();

    let mut server = ServerProcess::start(&[]);
    server.request(1, "initialize", json!({"rootUri": root_uri}));
    server.notify("initialized", json!({}));
    let mut saw_model = false;
    let mut registered_watcher = false;
    while !saw_model || !registered_watcher {
        let message = server.receive().expect("project configuration message");
        if message["method"] == "window/showMessage" {
            let text = message["params"]["message"].as_str().unwrap_or_default();
            assert!(
                !text.contains("could not restart analysis worker"),
                "oversized classpath must not enter the worker restart loop: {text}"
            );
        }
        if message["method"] == "window/logMessage" {
            saw_model |= message["params"]["message"]
                .as_str()
                .is_some_and(|text| text.contains("2752 classpath entries"));
        }
        if message["method"] == "client/registerCapability" {
            registered_watcher = true;
            server.send(&json!({
                "jsonrpc": "2.0",
                "id": message["id"],
                "result": null
            }));
        }
    }
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kotlin",
                "version": 1,
                "text": source
            }
        }),
    );
    assert!(server.await_diagnostics(&uri).is_empty());
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
fn stdio_server_reports_official_nullable_receiver_diagnostic_with_utf16_range() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///nullable.kt",
        "fun nullableMemberCall(value: String?): String = /*😀*/ value. /* gap */ substring(1)",
    );
    assert_eq!(
        diagnostics,
        vec![json!({
            "range": {
                "start": {"line": 0, "character": 61},
                "end": {"line": 0, "character": 62}
            },
            "severity": 1,
            "source": "Kotlin",
            "message": "Only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'."
        })]
    );
}

#[test]
fn stdio_server_accepts_expected_type_selected_callable_reference_overloads() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///callable-reference.kt",
        "interface Parser\n\
         interface JsonParser : Parser\n\
         fun JsonParser.decode(source: String): Any = source\n\
         fun Parser.decode(source: String): Any = source\n\
         fun Parser.decode(value: Int): Any = value\n\
         fun consume(decode: (String) -> Any) {}\n\
         fun valid(parser: JsonParser) { consume(parser::decode) }\n",
    );

    assert_eq!(
        diagnostics,
        [(2, 4, 14), (3, 4, 10), (4, 4, 10),].map(|(line, start, end)| {
            json!({
                "range": {
                    "start": {"line": line, "character": start},
                    "end": {"line": line, "character": end}
                },
                "severity": 2,
                "source": null,
                "message": "Receiver parameter is never used"
            })
        })
    );
}

#[test]
fn stdio_server_counts_receiver_bound_call_as_receiver_use() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///implicit-receiver-extension-call.kt",
        "class Box<out T : Any?> private constructor(private val value: Any?) {\n\
         \x20   companion object {\n\
         \x20       fun <T> Box<T>.getOrNull(): Int {\n\
         \x20           getOrDefault(null)\n\
         \x20           return 0\n\
         \x20       }\n\
         \x20       fun <T> Box<T>.getOrDefault(defaultValue: T): T = defaultValue\n\
         \x20   }\n\
         }\n",
    );
    assert_eq!(diagnostics, Vec::<Value>::new());
}

#[test]
fn stdio_server_visits_a_companion_extension_once() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///companion-extension.kt",
        "class Box {\n\
         \x20   companion object {\n\
         \x20       fun String.unused(): Int = 1\n\
         \x20   }\n\
         }\n",
    );
    assert_eq!(
        diagnostics,
        [json!({
            "range": {
                "start": {"line": 2, "character": 12},
                "end": {"line": 2, "character": 18}
            },
            "severity": 2,
            "source": null,
            "message": "Receiver parameter is never used"
        })]
    );
}

#[test]
fn stdio_server_counts_a_result_type_parameter_as_receiver_use() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///result-type-receiver.kt",
        "class Box<T>\nfun <T> Box<T>.empty(): T? = null\n",
    );
    assert_eq!(diagnostics, Vec::<Value>::new());
}

#[test]
fn stdio_server_accepts_adapted_callable_reference_during_generic_inference() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///generic-adapted-callable.kt",
        "fun foo(x: String, y: Char = 'K'): String = x + y\n\
         fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
         fun value(): String = call(::foo, \"O\")",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn stdio_server_reports_inapplicable_generic_adapted_reference_on_the_name() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///inapplicable-generic-adapted-callable.kt",
        "fun <T> applySame(block: (T) -> T, value: T): T = block(value)\n\
         fun mismatched(x: String, suffix: Char = 'K'): Int = x.length + suffix.code\n\
         fun bad(): Any = applySame(::mismatched, \"O\")",
    );
    assert_eq!(
        diagnostics,
        vec![json!({
            "range": {
                "start": {"line": 2, "character": 29},
                "end": {"line": 2, "character": 39}
            },
            "severity": 1,
            "source": "Kotlin",
            "message": "Inapplicable candidate(s): fun mismatched(x: String, suffix: Char = ...): Int"
        })]
    );
}

#[test]
fn stdio_server_reports_official_callable_reference_ambiguity_on_the_name() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///ambiguous-reference.kt",
        "class Parser\n\
         fun Parser.decode(source: String): String = source\n\
         fun Parser.decode(value: Int): String = value.toString()\n\
         fun bad() {\n\
             val parser = Parser()\n\
             val reference = parser::decode\n\
         }\n",
    );

    assert_eq!(
        diagnostics,
        vec![
            json!({
                "range": {
                    "start": {"line": 5, "character": 24},
                    "end": {"line": 5, "character": 30}
                },
                "severity": 1,
                "source": "Kotlin",
                "message": "Overload resolution ambiguity between candidates:\nfun Parser.decode(source: String): String\nfun Parser.decode(value: Int): String"
            }),
            json!({
                "range": {
                    "start": {"line": 1, "character": 4},
                    "end": {"line": 1, "character": 10}
                },
                "severity": 2,
                "source": null,
                "message": "Receiver parameter is never used"
            }),
            json!({
                "range": {
                    "start": {"line": 2, "character": 4},
                    "end": {"line": 2, "character": 10}
                },
                "severity": 2,
                "source": null,
                "message": "Receiver parameter is never used"
            }),
        ]
    );
}

#[test]
fn stdio_server_reports_official_toplevel_callable_reference_ambiguity_on_the_name() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///ambiguous-toplevel-reference.kt",
        "interface Text\n\
         class Narrow : Text\n\
         fun cross(x: Narrow, y: Any): String = \"A\"\n\
         fun cross(x: Text, y: Text): String = \"B\"\n\
         fun bad() { val reference: (Narrow, Narrow) -> String = ::cross }\n",
    );

    assert_eq!(
        diagnostics,
        vec![json!({
            "range": {
                "start": {"line": 4, "character": 58},
                "end": {"line": 4, "character": 63}
            },
            "severity": 1,
            "source": "Kotlin",
            "message": "Overload resolution ambiguity between candidates:\nfun cross(x: Narrow, y: Any): String\nfun cross(x: Text, y: Text): String"
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
fn stdio_server_reports_no_diagnostics_for_vararg_spread_and_named_shapes() {
    // Valid Kotlin vararg call shapes that used to surface false positives: a mixed
    // element + spread call on a vararg extension, and a named argument binding the
    // defaulted parameter after positional vararg elements. Both must produce NO diagnostics.
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///vararg_shapes.kt",
        "class B(val n: Int)\n\
         fun B.segd(vararg s: String, flag: Boolean = false): Int = n + s.size\n\
         fun topd(vararg s: String, flag: Boolean = false): Int = s.size\n\
         fun use(b: B, xs: Array<String>): Int = b.segd(\"a\", *xs) + topd(\"x\", \"y\", flag = true)",
    );
    assert_eq!(diagnostics, Vec::<Value>::new());
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
fn stdio_server_accepts_non_exhaustive_when_in_expected_unit_lambda() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///statement-when.kt",
        "fun consumeUnit(block: () -> Unit) { block() }\n\
         fun statementWhen(value: Int) {\n\
           when (value) { 1 -> println(value) }\n\
           consumeUnit { when (value) { 2 -> println(value) } }\n\
         }",
    );

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn stdio_server_reports_unused_extension_receiver_as_an_inspection() {
    let diagnostics = diagnostics_after_open(
        &[],
        "file:///unused-receiver.kt",
        "fun String.unused(value: Int): Int = value",
    );

    assert_eq!(
        diagnostics,
        vec![json!({
            "range": {
                "start": {"line": 0, "character": 4},
                "end": {"line": 0, "character": 10}
            },
            "severity": 2,
            "source": null,
            "message": "Receiver parameter is never used"
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
fn stdio_server_finds_workspace_symbols_in_files_nothing_opened() {
    let project = TempProject::new("workspace-symbol-coverage");
    project.write(
        "src/Never.kt",
        "package sample\nclass NeverOpenedMarker {\n  fun neverOpenedMember(): Int = 1\n}\n",
    );
    let open_uri = project.uri("src/Open.kt");
    let root_uri: String = url::Url::from_directory_path(project.path())
        .expect("temporary project root is a file URI")
        .into();

    let mut server = ServerProcess::start(&[]);
    server.request(1, "initialize", json!({"rootUri": root_uri}));
    server.notify("initialized", json!({}));
    server.receive_until(|message| message["method"] == "client/registerCapability");
    // The background sweep is raised only once an interactive analysis has been served, so the
    // session has to look like a real one before project-wide coverage starts.
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": open_uri,
                "languageId": "kotlin",
                "version": 1,
                "text": "package sample\nfun opened(): Int = 1\n"
            }
        }),
    );
    let diagnostics = server.await_diagnostics(&open_uri);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    // Indexing is asynchronous, and the picker re-queries on every keystroke, so polling is what a
    // client does too.
    let mut found = Value::Null;
    for attempt in 0..200 {
        let response = server.request(
            100 + attempt,
            "workspace/symbol",
            json!({"query": "NeverOpenedMarker"}),
        );
        if response["result"][0].is_object() {
            found = response["result"][0].clone();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(
        found["location"]["uri"],
        project.uri("src/Never.kt"),
        "a declaration in a file nobody opened must be findable"
    );
    assert_eq!(found["name"], "NeverOpenedMarker");
    assert_eq!(found["location"]["range"]["start"]["line"], 1);

    let member = server.request(
        200,
        "workspace/symbol",
        json!({"query": "neverOpenedMember"}),
    );
    assert_eq!(
        member["result"][0]["location"]["uri"],
        project.uri("src/Never.kt")
    );
    assert_eq!(
        member["result"][0]["containerName"],
        "sample.NeverOpenedMarker"
    );

    server.shutdown_and_exit();
}

#[test]
fn stdio_server_locates_a_dependency_workspace_symbol_through_the_real_worker() {
    let project = TempProject::new("dependency-workspace-symbol");
    project.write(
        ".idea/modules.xml",
        r#"<project><component name="ProjectModuleManager"><modules>
             <module filepath="$PROJECT_DIR$/app/app.iml" />
           </modules></component></project>"#,
    );
    project.write(
        "app/app.iml",
        r#"<module><component name="NewModuleRootManager">
             <content url="file://$MODULE_DIR$">
               <sourceFolder url="file://$MODULE_DIR$/src" isTestSource="false" />
             </content>
             <orderEntry type="library" name="dependency" level="project" />
           </component></module>"#,
    );
    project.write(
        ".idea/libraries/dependency.xml",
        r#"<component name="libraryTable"><library name="dependency"><CLASSES>
             <root url="file://$PROJECT_DIR$/dependencies/classes" />
           </CLASSES></library></component>"#,
    );
    let class = project
        .path()
        .join("dependencies/classes/vendor/DependencyMarker.class");
    std::fs::create_dir_all(class.parent().unwrap()).expect("create dependency package");
    std::fs::write(
        &class,
        ClassWriter::new("vendor/DependencyMarker", "java/lang/Object").finish(),
    )
    .expect("write dependency class");
    let open_uri = project.uri("app/src/Open.kt");
    let root_uri: String = url::Url::from_directory_path(project.path())
        .expect("temporary project root is a file URI")
        .into();
    let cache = project.path().join("dependency-cache");
    let cache = cache.to_string_lossy().into_owned();

    let mut server = ServerProcess::start(&["-deps-cache-dir", &cache]);
    server.request(1, "initialize", json!({"rootUri": root_uri}));
    server.notify("initialized", json!({}));
    server.receive_until(|message| message["method"] == "client/registerCapability");
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": open_uri,
                "languageId": "kotlin",
                "version": 1,
                "text": "package app\nfun opened(): Int = 1\n"
            }
        }),
    );
    assert!(server.await_diagnostics(&open_uri).is_empty());

    // The first matching query schedules worker materialization and answers without blocking. Poll
    // exactly as a picker does so this covers the production WorkerHost transport, engine event,
    // content-addressed write, and the next-query convergence rather than injecting an index or a
    // located result into the service.
    let mut found = Value::Null;
    for attempt in 0..200 {
        let response = server.request(
            300 + attempt,
            "workspace/symbol",
            json!({"query": "DependencyMarker"}),
        );
        if response["result"][0].is_object() {
            found = response["result"][0].clone();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(found["name"], "DependencyMarker");
    assert_eq!(found["containerName"], "vendor");
    let rendered_uri = found["location"]["uri"]
        .as_str()
        .expect("dependency symbol has a URI");
    let rendered_path = url::Url::parse(rendered_uri)
        .unwrap()
        .to_file_path()
        .expect("dependency URI is a local cache file");
    let rendered = std::fs::read_to_string(rendered_path).expect("rendered dependency source");
    assert!(rendered.contains("class DependencyMarker"));
    assert_eq!(found["location"]["range"]["start"]["line"], 2);

    server.shutdown_and_exit();
}

#[test]
fn stdio_server_reports_official_cross_file_conflicting_overloads() {
    let project = TempProject::new("conflicting-overloads");
    project.write(
        "src/OtherOne.kt",
        "fun namedPair(left: Int, right: String): String = right\n",
    );
    project.write(
        "src/OtherTwo.kt",
        "fun namedPair(left: Int, right: String): String = right\n",
    );
    let target = "fun namedPair(left: Int, right: String): Int = left\n\
                  fun missingNamedArgument(): Int = namedPair(left = 1)\n";
    let target_uri = project.uri("src/Target.kt");
    let root_uri: String = url::Url::from_directory_path(project.path())
        .expect("temporary project root is a file URI")
        .into();

    let mut server = ServerProcess::start(&[]);
    server.request(1, "initialize", json!({"rootUri": root_uri}));
    server.notify("initialized", json!({}));
    server.receive_until(|message| message["method"] == "client/registerCapability");
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": target_uri,
                "languageId": "kotlin",
                "version": 1,
                "text": target
            }
        }),
    );

    assert_eq!(
        server.await_diagnostics(&target_uri),
        vec![
            json!({
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 44}
                },
                "severity": 1,
                "source": "Kotlin",
                "message": "Conflicting overloads:\n\
                            fun namedPair(left: Int, right: String): String\n\
                            fun namedPair(left: Int, right: String): String"
            }),
            json!({
                "range": {
                    "start": {"line": 1, "character": 34},
                    "end": {"line": 1, "character": 43}
                },
                "severity": 1,
                "source": "Kotlin",
                "message": "No value passed for parameter 'right'."
            }),
            json!({
                "range": {
                    "start": {"line": 1, "character": 34},
                    "end": {"line": 1, "character": 43}
                },
                "severity": 1,
                "source": "Kotlin",
                "message": "None of the following candidates is applicable:\n\n\
                            fun namedPair(left: Int, right: String): Int\n\
                            fun namedPair(left: Int, right: String): String\n\
                            fun namedPair(left: Int, right: String): String"
            }),
        ]
    );
    server.shutdown_and_exit();
}

#[test]
fn stdio_server_merges_navigation_across_dependent_modules() {
    let project = TempProject::new("module-navigation");
    project.write(
        ".idea/modules.xml",
        r#"<project><component name="ProjectModuleManager"><modules>
             <module filepath="$PROJECT_DIR$/base/base.iml" />
             <module filepath="$PROJECT_DIR$/first/first.iml" />
             <module filepath="$PROJECT_DIR$/second/second.iml" />
             <module filepath="$PROJECT_DIR$/empty/empty.iml" />
           </modules></component></project>"#,
    );
    project.write(
        "base/base.iml",
        r#"<module><component name="NewModuleRootManager">
             <content url="file://$MODULE_DIR$">
               <sourceFolder url="file://$MODULE_DIR$/src" isTestSource="false" />
             </content>
           </component></module>"#,
    );
    for module in ["first", "second", "empty"] {
        project.write(
            &format!("{module}/{module}.iml"),
            r#"<module><component name="NewModuleRootManager">
                 <content url="file://$MODULE_DIR$">
                   <sourceFolder url="file://$MODULE_DIR$/src" isTestSource="false" />
                 </content>
                 <orderEntry type="module" module-name="base" />
               </component></module>"#,
        );
    }
    let base_source = "package sample\nopen class Base\nfun token(): Int = 1\n";
    let first_source = "package sample\nclass First : Base()\nfun firstUse(): Int = token()\n";
    let second_source = "package sample\nclass Second : Base()\nfun secondUse(): Int = token()\n";
    let empty_source = "";
    let hidden_source = "package sample\nclass Hidden : Base()\n";
    let base_uri = project.uri("base/src/Base.kt");
    let first_uri = project.uri("first/src/First.kt");
    let second_uri = project.uri("second/src/Second.kt");
    let empty_uri = project.uri("empty/src/Open.kt");
    let hidden_uri = project.uri("empty/src/Hidden.kt");
    project.write("base/src/Base.kt", base_source);
    project.write("first/src/First.kt", first_source);
    project.write("second/src/Second.kt", second_source);
    project.write("empty/src/Open.kt", empty_source);
    project.write("empty/src/Hidden.kt", hidden_source);
    let root_uri: String = url::Url::from_directory_path(project.path())
        .expect("temporary project root is a file URI")
        .into();
    let mut server = ServerProcess::start(&[]);
    server.request(1, "initialize", json!({"rootUri": root_uri}));
    server.notify("initialized", json!({}));
    server.receive_until(|message| message["method"] == "client/registerCapability");
    for (uri, text) in [
        (&base_uri, base_source),
        (&first_uri, first_source),
        (&second_uri, second_source),
        (&empty_uri, empty_source),
    ] {
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
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
    let implementation = server.request(
        2,
        "textDocument/implementation",
        json!({
            "textDocument": {"uri": base_uri},
            "position": {"line": 1, "character": 12}
        }),
    );
    let references = server.request(
        3,
        "textDocument/references",
        json!({
            "textDocument": {"uri": base_uri},
            "position": {"line": 2, "character": 5},
            "context": {"includeDeclaration": true}
        }),
    );
    let rename = server.request(
        4,
        "textDocument/rename",
        json!({
            "textDocument": {"uri": second_uri},
            "position": {"line": 2, "character": 24},
            "newName": "renamed"
        }),
    );
    let response_uris = |response: &Value| {
        response["result"]
            .as_array()
            .unwrap_or_else(|| panic!("response: {response}"))
            .iter()
            .map(|location| location["uri"].as_str().unwrap().to_string())
            .collect::<std::collections::HashSet<_>>()
    };
    assert_eq!(
        response_uris(&implementation),
        std::collections::HashSet::from([
            first_uri.clone(),
            second_uri.clone(),
            hidden_uri.clone()
        ])
    );
    assert_eq!(
        response_uris(&references),
        std::collections::HashSet::from([base_uri.clone(), first_uri.clone(), second_uri.clone()])
    );
    let changed_uris = rename["result"]["documentChanges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["textDocument"]["uri"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        changed_uris,
        std::collections::HashSet::from([
            base_uri.as_str(),
            first_uri.as_str(),
            second_uri.as_str()
        ])
    );
    server.shutdown_and_exit();
}

#[test]
fn stdio_server_keeps_dependency_and_friend_visibility_distinct() {
    let project = TempProject::new("module-visibility");
    project.write(
        ".idea/modules.xml",
        r#"<project><component name="ProjectModuleManager"><modules>
             <module filepath="$PROJECT_DIR$/producer/producer.iml" />
             <module filepath="$PROJECT_DIR$/consumer/consumer.iml" />
             <module filepath="$PROJECT_DIR$/isolated/isolated.iml" />
           </modules></component></project>"#,
    );
    project.write(
        ".idea/misc.xml",
        r#"<project><component name="ProjectRootManager">
             <output url="file://$PROJECT_DIR$/out" />
           </component></project>"#,
    );
    project.write(
        "producer/producer.iml",
        r#"<module><component name="NewModuleRootManager" inherit-compiler-output="true">
             <content url="file://$MODULE_DIR$">
               <sourceFolder url="file://$MODULE_DIR$/src/main" isTestSource="false" />
               <sourceFolder url="file://$MODULE_DIR$/src/test" isTestSource="true" />
             </content>
           </component></module>"#,
    );
    project.write(
        "consumer/consumer.iml",
        r#"<module><component name="NewModuleRootManager" inherit-compiler-output="true">
             <content url="file://$MODULE_DIR$">
               <sourceFolder url="file://$MODULE_DIR$/src" isTestSource="false" />
             </content>
             <orderEntry type="module" module-name="producer" />
           </component></module>"#,
    );
    project.write(
        "isolated/isolated.iml",
        r#"<module><component name="NewModuleRootManager" inherit-compiler-output="true">
             <content url="file://$MODULE_DIR$">
               <sourceFolder url="file://$MODULE_DIR$/src" isTestSource="false" />
             </content>
           </component></module>"#,
    );
    let open_internal = "package fixture\ninternal class OpenInternal\nclass OpenPublic\n";
    let friend_use = "package fixture\n\
                      fun openFriend(): Any = OpenInternal()\n\
                      fun diskFriend(): Any = DiskInternal()\n";
    let dependency_use = "package fixture\n\
                          fun openDependency(): Any = OpenInternal()\n\
                          fun diskDependency(): Any = DiskInternal()\n";
    project.write(
        "producer/src/main/DiskInternal.kt",
        "package fixture\ninternal class DiskInternal\n",
    );
    project.write(
        "isolated/src/Isolated.kt",
        "package fixture\nclass IsolatedPublic\n",
    );
    let open_internal_uri = project.uri("producer/src/main/OpenInternal.kt");
    let friend_uri = project.uri("producer/src/test/FriendUse.kt");
    let dependency_uri = project.uri("consumer/src/DependencyUse.kt");
    let root_uri: String = url::Url::from_directory_path(project.path())
        .expect("temporary project root is a file URI")
        .into();
    let mut server = ServerProcess::start(&[]);
    server.request(1, "initialize", json!({"rootUri": root_uri}));
    server.notify("initialized", json!({}));
    server.receive_until(|message| message["method"] == "client/registerCapability");
    for (uri, text) in [
        (&open_internal_uri, open_internal),
        (&friend_uri, friend_use),
    ] {
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
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": dependency_uri,
                "languageId": "kotlin",
                "version": 1,
                "text": dependency_use
            }
        }),
    );
    let mut diagnostics =
        server.await_diagnostics_for(&[friend_uri.as_str(), dependency_uri.as_str()]);
    let friend_diagnostics = diagnostics.remove(&friend_uri).unwrap();
    assert!(friend_diagnostics.is_empty(), "{friend_diagnostics:?}");
    let dependency_diagnostics = diagnostics.remove(&dependency_uri).unwrap();
    assert_eq!(
        dependency_diagnostics.len(),
        2,
        "{dependency_diagnostics:?}"
    );
    let messages: std::collections::HashSet<_> = dependency_diagnostics
        .iter()
        .map(|diagnostic| diagnostic["message"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        messages,
        std::collections::HashSet::from([
            "Cannot access 'OpenInternal': it is internal",
            "Cannot access 'DiskInternal': it is internal",
        ])
    );

    let completion_labels = |response: &Value| {
        response["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["label"].as_str())
            .map(str::to_owned)
            .collect::<std::collections::HashSet<_>>()
    };
    let friend_completion = server.request(
        2,
        "textDocument/completion",
        json!({
            "textDocument": {"uri": friend_uri},
            "position": {"line": 1, "character": 26}
        }),
    );
    let friend_labels = completion_labels(&friend_completion);
    assert!(friend_labels.contains("OpenPublic"));
    assert!(friend_labels.contains("OpenInternal"));
    assert!(friend_labels.contains("DiskInternal"));
    assert!(!friend_labels.contains("IsolatedPublic"));

    let dependency_completion = server.request(
        3,
        "textDocument/completion",
        json!({
            "textDocument": {"uri": dependency_uri},
            "position": {"line": 1, "character": 30}
        }),
    );
    let dependency_labels = completion_labels(&dependency_completion);
    assert!(dependency_labels.contains("OpenPublic"));
    assert!(!dependency_labels.contains("OpenInternal"));
    assert!(!dependency_labels.contains("DiskInternal"));
    assert!(!dependency_labels.contains("IsolatedPublic"));
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
                        .any(|diagnostic| diagnostic["message"] == "Unresolved reference 'Item'.")
                })
    });
    assert!(changed["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|diagnostic| diagnostic["message"] == "Unresolved reference 'Item'."));

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
    assert_eq!(
        diagnostics[0]["message"].as_str(),
        Some("Module source set exceeds analysis limit (maximum 32 MiB); semantic diagnostics suppressed")
    );
    server.shutdown_and_exit();
}

/// Opens one document and immediately asks for the dev dump action.
///
/// All three coverage cases use the same wire sequence. Keeping that sequence here makes their
/// differences explicit: initialization capabilities and workspace rooting belong to each test,
/// while document synchronization and the code-action request must stay structurally equivalent.
/// Deliberately do not wait for a diagnostic publish: the action itself must wait for the current
/// analysis, otherwise a fast editor request can race the retained dump input and return no action.
fn request_dump_code_action(
    server: &mut ServerProcess,
    request_id: i64,
    uri: &str,
    text: &str,
) -> Value {
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
    server.request(
        request_id,
        "textDocument/codeAction",
        json!({
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 0, "character": 0},
                      "end": {"line": 0, "character": 0}},
            "context": {"diagnostics": []}
        }),
    )
}

const DUMP_ACTION_SOURCE: &str = "fun box(): String = \"OK\"\n";

#[test]
fn stdio_server_offers_the_dev_dump_code_action_for_a_rootless_document() {
    let uri = "file:///dev-dump/Main.kt";
    // An initialize request without an explicit root deliberately falls back to the server's
    // current directory. Give this process an empty directory: inheriting the repository would
    // make the supposedly rootless fixture discover transient Kotlin files produced by parallel
    // tests, so unrelated corpus size could consume the bounded dump-retention budget.
    let isolated_cwd = TempProject::new("dev-dump-rootless-cwd");
    let cache = isolated_cwd.path().join("cache");
    let cache = cache.to_str().expect("UTF-8 temporary cache path");
    let mut server =
        ServerProcess::start_in(&["--dev", "-deps-cache-dir", cache], isolated_cwd.path());
    let initialize = server.request(1, "initialize", json!({}));
    let capabilities = &initialize["result"]["capabilities"];
    assert_eq!(
        capabilities["codeActionProvider"],
        json!(true),
        "dev mode must advertise the code action capability: {initialize}"
    );
    // Advertising the provider is not sufficient. Zed discards a returned code action whose
    // `command` is missing from `executeCommandProvider.commands` before the action reaches the
    // menu, and reports nothing — the user sees "no code actions available" from a server that
    // answered correctly. The command itself is handled by the editor and never sent back here.
    let advertised = capabilities["executeCommandProvider"]["commands"]
        .as_array()
        .expect("dev mode must advertise the executeCommandProvider commands");
    assert!(
        advertised
            .iter()
            .any(|command| command == "editor.action.goToLocations"),
        "the action's command must be advertised or the editor drops the action: {initialize}"
    );
    server.notify("initialized", json!({}));
    let response = request_dump_code_action(&mut server, 2, uri, DUMP_ACTION_SOURCE);
    let actions = response["result"]
        .as_array()
        .expect("code action array result");
    assert_eq!(actions.len(), 1, "expected the dump action: {response}");

    let command = &actions[0]["command"];
    assert_eq!(command["command"], "editor.action.goToLocations");
    // Zed reads arguments[2] as the location list and ignores the first two, but bails when the
    // array holds fewer than three entries — turning the action into a silent no-op with no error
    // anywhere. The in-crate tests pin this shape; this one proves it survives the real binary.
    let arguments = command["arguments"]
        .as_array()
        .expect("command arguments array");
    assert_eq!(arguments.len(), 3, "{command}");
    assert_eq!(arguments[0], uri);
    let locations = arguments[2].as_array().expect("location array");
    assert_eq!(locations.len(), 1, "{command}");
    assert!(
        locations[0]["uri"]
            .as_str()
            .is_some_and(|uri| uri.ends_with(".krusty.md")),
        "the action must point at a written dump: {command}"
    );
    server.shutdown_and_exit();
}

#[test]
fn stdio_server_offers_no_code_actions_without_dev_mode() {
    // The dump is invisible unless the server was started with `--dev`. Without coverage here, a
    // correctly built server reads as a broken one: the editor just reports no code actions.
    let uri = "file:///dev-dump/Main.kt";
    let mut server = ServerProcess::start(&[]);
    let initialize = server.request(1, "initialize", json!({}));
    assert!(
        initialize["result"]["capabilities"]
            .get("codeActionProvider")
            .is_none(),
        "a non-dev server must not advertise the capability: {initialize}"
    );
    assert!(
        initialize["result"]["capabilities"]
            .get("executeCommandProvider")
            .is_none(),
        "a non-dev server must not advertise dump commands either: {initialize}"
    );
    server.notify("initialized", json!({}));
    let response = request_dump_code_action(&mut server, 2, uri, DUMP_ACTION_SOURCE);
    assert_eq!(
        response["result"],
        json!([]),
        "a non-dev server must answer with an empty action list: {response}"
    );
    server.shutdown_and_exit();
}

#[test]
fn stdio_server_offers_the_dev_dump_code_action_in_a_project() {
    let project = TempProject::new("dev-dump-project");
    project.write("src/Main.kt", "fun box(): String = \"OK\"\n");
    let uri = project.uri("src/Main.kt");
    let root_uri: String = url::Url::from_directory_path(project.path())
        .expect("temporary project root is a file URI")
        .into();
    let cache = project.path().join("cache");
    let cache = cache.to_str().expect("UTF-8 temporary cache path");

    let mut server = ServerProcess::start(&["--dev", "-deps-cache-dir", cache]);
    server.request(1, "initialize", json!({"rootUri": root_uri}));
    server.notify("initialized", json!({}));
    let response = request_dump_code_action(&mut server, 2, &uri, DUMP_ACTION_SOURCE);
    let actions = response["result"]
        .as_array()
        .expect("code action array result");
    assert_eq!(actions.len(), 1, "expected the dump action: {response}");
    server.shutdown_and_exit();
}
