//! Opt-in diagnostics, highlighting, navigation, hover, and completion differential against
//! JetBrains' official Kotlin LSP.
//!
//! Set `KRUSTY_KOTLIN_LSP` to the official launcher (`kotlin-lsp.sh` or
//! `bin/intellij-server`). The official distribution is intentionally not downloaded by the normal
//! test suite: it is large, platform-specific, and released independently from krusty.

mod common;

use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use krusty_lsp::{read_framed, write_framed, MAX_MESSAGE_BYTES};
use serde_json::{json, Value};

use common::TempProject;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const ANALYSIS_TIMEOUT: Duration = Duration::from_secs(300);
static OFFICIAL_DIFFERENTIAL: Mutex<()> = Mutex::new(());

fn reference_version_key(version: &str) -> [u32; 3] {
    let mut parts = version.split('.');
    let key = [
        parts.next().and_then(|part| part.parse().ok()),
        parts.next().and_then(|part| part.parse().ok()),
        parts.next().and_then(|part| part.parse().ok()),
    ];
    assert!(
        parts.next().is_none() && key.iter().all(Option::is_some),
        "invalid reference version {version:?} in kotlin-versions"
    );
    key.map(Option::unwrap)
}

fn reference_kotlin_version_from(manifest: &str) -> &str {
    manifest
        .lines()
        .filter_map(|line| {
            let line = line.split('#').next()?.trim();
            if line.is_empty() {
                None
            } else {
                line.split_whitespace().next()
            }
        })
        .max_by_key(|version| reference_version_key(version))
        .expect("kotlin-versions must contain a reference version")
}

fn reference_kotlin_version() -> &'static str {
    reference_kotlin_version_from(include_str!("../../../kotlin-versions"))
}

struct SemanticLegend {
    types: Vec<String>,
    modifiers: Vec<String>,
}

struct LspProcess {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    pending: Vec<Value>,
    next_request_id: i64,
}

impl LspProcess {
    fn spawn(program: &str, args: &[&str]) -> Self {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|error| panic!("start LSP {program:?}: {error}"));
        let stdin = child.stdin.take().expect("LSP stdin");
        let stdout = child.stdout.take().expect("LSP stdout");
        let (send, messages) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            while let Ok(Some(body)) = read_framed(&mut stdout, MAX_MESSAGE_BYTES) {
                let Ok(message) = serde_json::from_slice(&body) else {
                    break;
                };
                if send.send(message).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            messages,
            pending: Vec::new(),
            next_request_id: 2,
        }
    }

    fn send(&mut self, message: Value) {
        write_framed(&mut self.stdin, &serde_json::to_vec(&message).unwrap()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn respond_to_server_request(&mut self, message: &Value) -> bool {
        let (Some(id), Some(method)) = (message.get("id"), message["method"].as_str()) else {
            return false;
        };
        let result = match method {
            "workspace/configuration" => {
                let count = message["params"]["items"].as_array().map_or(0, Vec::len);
                Value::Array(vec![Value::Null; count])
            }
            "workspace/workspaceFolders" => json!([]),
            "workspace/applyEdit" => json!({"applied": false}),
            _ => Value::Null,
        };
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
        true
    }

    fn receive_until(&mut self, deadline: Instant, predicate: impl Fn(&Value) -> bool) -> Value {
        if let Some(index) = self.pending.iter().position(&predicate) {
            return self.pending.swap_remove(index);
        }
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("timed out waiting for LSP response");
            let message = self
                .messages
                .recv_timeout(remaining)
                .expect("timed out waiting for LSP response");
            if self.respond_to_server_request(&message) {
                continue;
            }
            if predicate(&message) {
                return message;
            }
            self.pending.push(message);
        }
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        self.receive_until(Instant::now() + REQUEST_TIMEOUT, |message| {
            message["id"] == id
        })
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn open_document(&mut self, uri: &str, text: &str) {
        self.notify(
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
    }

    fn next_request_id(&mut self) -> i64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    fn change_document(&mut self, uri: &str, version: i64, content_changes: Value) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": content_changes
            }),
        );
    }

    fn initialize(&mut self, root_uri: &str) -> SemanticLegend {
        let response = self.request(
            1,
            "initialize",
            json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "diagnostic": {},
                        "documentSymbol": {
                            "hierarchicalDocumentSymbolSupport": true
                        },
                        "publishDiagnostics": {"versionSupport": true},
                        "semanticTokens": {
                            "requests": {"full": true},
                            "tokenTypes": [
                                "namespace", "type", "class", "enum", "interface", "struct",
                                "typeParameter", "parameter", "variable", "property", "enumMember",
                                "event", "function", "method", "macro", "label", "comment",
                                "string", "keyword", "number", "regexp", "operator", "decorator"
                            ],
                            "tokenModifiers": [
                                "declaration", "definition", "readonly", "static", "deprecated",
                                "abstract", "async", "modification", "documentation", "defaultLibrary"
                            ],
                            "formats": ["relative"]
                        }
                    },
                    "workspace": {"workspaceFolders": true}
                },
                "workspaceFolders": [{"uri": root_uri, "name": "krusty-lsp-diff"}]
            }),
        );
        assert!(
            response.get("result").is_some(),
            "initialize failed: {response}"
        );
        assert_eq!(
            response["result"]["capabilities"]["textDocumentSync"], 2,
            "LSP must advertise the official server's incremental synchronization mode"
        );
        assert_eq!(
            response["result"]["capabilities"]["diagnosticProvider"],
            json!({
                "interFileDependencies": true,
                "workspaceDiagnostics": false,
                "workDoneProgress": false
            }),
            "LSP must advertise the official server's pull-diagnostic contract"
        );
        assert_eq!(
            response["result"]["capabilities"]["foldingRangeProvider"], true,
            "LSP must advertise the official server's folding-range contract"
        );
        assert_eq!(
            response["result"]["capabilities"]["renameProvider"], true,
            "LSP must advertise the official server's rename contract"
        );
        assert_eq!(
            response["result"]["capabilities"]["typeDefinitionProvider"], true,
            "LSP must advertise the official server's type-definition contract"
        );
        assert_eq!(
            response["result"]["capabilities"]["implementationProvider"], true,
            "LSP must advertise the official server's implementation contract"
        );
        self.notify("initialized", json!({}));
        SemanticLegend {
            types: response["result"]["capabilities"]["semanticTokensProvider"]["legend"]
                ["tokenTypes"]
                .as_array()
                .expect("semantic token type legend")
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect(),
            modifiers: response["result"]["capabilities"]["semanticTokensProvider"]["legend"]
                ["tokenModifiers"]
                .as_array()
                .expect("semantic token modifier legend")
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect(),
        }
    }

    fn diagnostics(&mut self, uri: &str, text: &str) -> Vec<Value> {
        self.diagnostics_at_least(uri, text, 1)
    }

    fn diagnostics_at_least(&mut self, uri: &str, text: &str, minimum: usize) -> Vec<Value> {
        self.open_document(uri, text);

        // The first opt-in run may need to download Gradle and import/index a cold project.
        let deadline = Instant::now() + ANALYSIS_TIMEOUT;
        loop {
            let items = self.pull_diagnostics(uri);
            if items.len() >= minimum {
                return items;
            }
            assert!(
                Instant::now() < deadline,
                "LSP produced fewer than {minimum} diagnostics for {uri}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn diagnostics_allow_empty(&mut self, uri: &str, text: &str) -> Vec<Value> {
        self.open_document(uri, text);
        self.pull_diagnostics(uri)
    }

    fn pull_diagnostics(&mut self, uri: &str) -> Vec<Value> {
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/diagnostic",
            json!({"textDocument": {"uri": uri}}),
        );
        assert_eq!(
            response["result"]["kind"], "full",
            "LSP pull diagnostics must use the official full-report shape"
        );
        response["result"]["items"]
            .as_array()
            .expect("LSP full diagnostic report items")
            .clone()
    }

    fn semantic_tokens(&mut self, uri: &str, text: &str, legend: &SemanticLegend) -> Vec<Value> {
        self.open_document(uri, text);
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/semanticTokens/full",
            json!({"textDocument": {"uri": uri}}),
        );
        let data = response["result"]["data"]
            .as_array()
            .expect("semantic token data");
        assert_eq!(data.len() % 5, 0, "invalid semantic token stream");
        let mut line = 0u64;
        let mut character = 0u64;
        data.chunks_exact(5)
            .map(|token| {
                let delta_line = token[0].as_u64().unwrap();
                let delta_character = token[1].as_u64().unwrap();
                if delta_line == 0 {
                    character += delta_character;
                } else {
                    line += delta_line;
                    character = delta_character;
                }
                let token_type = token[3].as_u64().unwrap() as usize;
                let modifier_bits = token[4].as_u64().unwrap();
                let modifiers = legend
                    .modifiers
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| modifier_bits & (1 << index) != 0)
                    .map(|(_, modifier)| modifier)
                    .collect::<Vec<_>>();
                json!({
                    "line": line,
                    "character": character,
                    "length": token[2],
                    "type": legend.types.get(token_type).expect("semantic token type"),
                    "modifiers": modifiers,
                })
            })
            .collect()
    }

    fn definition(&mut self, uri: &str, line: u32, character: u32) -> Value {
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character}
            }),
        );
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    fn type_definition(&mut self, uri: &str, line: u32, character: u32) -> Value {
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/typeDefinition",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character}
            }),
        );
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    fn implementation(&mut self, uri: &str, line: u32, character: u32) -> Value {
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/implementation",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character}
            }),
        );
        let Some(locations) = response.get("result").and_then(Value::as_array) else {
            return Value::Null;
        };
        let mut locations = locations.clone();
        locations.sort_by_key(Value::to_string);
        Value::Array(locations)
    }

    fn document_symbols(&mut self, uri: &str) -> Value {
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        );
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    fn folding_ranges(&mut self, uri: &str) -> Value {
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/foldingRange",
            json!({"textDocument": {"uri": uri}}),
        );
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    fn signature_help(&mut self, uri: &str, line: u32, character: u32) -> Value {
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/signatureHelp",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character}
            }),
        );
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    fn references(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Value {
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/references",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
                "context": {"includeDeclaration": include_declaration}
            }),
        );
        let Some(locations) = response.get("result").and_then(Value::as_array) else {
            return Value::Null;
        };
        let mut locations = locations.clone();
        locations.sort_by_key(Value::to_string);
        Value::Array(locations)
    }

    fn rename(&mut self, uri: &str, line: u32, character: u32, new_name: &str) -> Value {
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/rename",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
                "newName": new_name
            }),
        );
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    fn hover(&mut self, uri: &str, line: u32, character: u32) -> Value {
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/hover",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character}
            }),
        );
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    fn completions(&mut self, uri: &str, line: u32, character: u32, label_prefix: &str) -> Value {
        let request_id = self.next_request_id();
        let response = self.request(
            request_id,
            "textDocument/completion",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character}
            }),
        );
        let result = response.get("result").cloned().unwrap_or(Value::Null);
        let is_incomplete = result["isIncomplete"].as_bool().unwrap_or(false);
        let items = result
            .as_array()
            .cloned()
            .or_else(|| result["items"].as_array().cloned())
            .unwrap_or_default();
        let mut items = items
            .into_iter()
            .filter(|item| {
                item["label"]
                    .as_str()
                    .is_some_and(|label| label.starts_with(label_prefix))
            })
            .map(|item| {
                let request_id = self.next_request_id();
                let response = self.request(request_id, "completionItem/resolve", item.clone());
                let resolved = response
                    .get("result")
                    .filter(|result| result.is_object())
                    .unwrap_or(&item);
                json!({
                    "label": resolved["label"],
                    "kind": resolved["kind"],
                    "labelDetails": resolved["labelDetails"],
                    "sortText": resolved["sortText"],
                })
            })
            .collect::<Vec<_>>();
        items.sort_by_key(Value::to_string);
        json!({
            "isIncomplete": is_incomplete,
            "items": items,
        })
    }
}

impl Drop for LspProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn normalized_diagnostics(diagnostics: Vec<Value>) -> Vec<Value> {
    let mut diagnostics = diagnostics
        .into_iter()
        .map(|diagnostic| {
            let message = diagnostic["message"]
                .as_str()
                .map(normalized_diagnostic_message)
                .map(Value::String)
                .unwrap_or_else(|| diagnostic["message"].clone());
            json!({
                "range": diagnostic["range"],
                "severity": diagnostic["severity"],
                "source": diagnostic["source"],
                "message": message,
            })
        })
        .collect::<Vec<_>>();
    diagnostics.sort_by_key(Value::to_string);
    diagnostics
}

fn normalized_diagnostic_message(message: &str) -> String {
    for prefix in [
        "Conflicting overloads:\n",
        "None of the following candidates is applicable:\n\n",
    ] {
        if let Some(candidates) = message.strip_prefix(prefix) {
            if candidates.is_empty()
                || candidates.ends_with('\n')
                || candidates.lines().any(str::is_empty)
            {
                return message.to_string();
            }
            let mut candidates = candidates.lines().collect::<Vec<_>>();
            candidates.sort_unstable();
            return format!("{prefix}{}", candidates.join("\n"));
        }
    }
    message.to_string()
}

fn position_after_marker(source: &str, marker: &str) -> (u32, u32) {
    let offset = source
        .find(marker)
        .unwrap_or_else(|| panic!("missing marker {marker:?}"))
        + marker.len();
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
    let character = prefix[line_start..].encode_utf16().count() as u32;
    (line, character)
}

#[test]
fn reference_version_selection_is_semantic_and_order_independent() {
    let manifest = "2.10.0 newer\n# ignored\n2.9.9 older\n";
    assert_eq!(reference_kotlin_version_from(manifest), "2.10.0");
}

#[test]
fn diagnostic_comparison_preserves_the_exact_range_and_text() {
    let diagnostics = normalized_diagnostics(vec![json!({
        "range": {
            "start": {"line": 3, "character": 5},
            "end": {"line": 3, "character": 12}
        },
        "severity": 1,
        "source": "Kotlin",
        "message": "Argument type mismatch.",
        "code": "ignored-by-parity-contract"
    })]);

    assert_eq!(
        diagnostics,
        vec![json!({
            "range": {
                "start": {"line": 3, "character": 5},
                "end": {"line": 3, "character": 12}
            },
            "severity": 1,
            "source": "Kotlin",
            "message": "Argument type mismatch."
        })]
    );
}

#[test]
fn diagnostic_comparison_canonicalizes_only_overload_candidate_order() {
    assert_eq!(
        normalized_diagnostic_message(
            "None of the following candidates is applicable:\n\nfun pick(): String\nfun pick(): Int"
        ),
        "None of the following candidates is applicable:\n\nfun pick(): Int\nfun pick(): String"
    );
    assert_eq!(
        normalized_diagnostic_message("Argument type mismatch."),
        "Argument type mismatch."
    );
    assert_ne!(
        normalized_diagnostic_message(
            "None of the following candidates is applicable:\n\nfun pick(): String\n"
        ),
        normalized_diagnostic_message(
            "None of the following candidates is applicable:\n\nfun pick(): String"
        )
    );
}

#[test]
fn cross_file_conflicting_overloads_match_official_kotlin_lsp() {
    let Ok(kotlin_lsp) = std::env::var("KRUSTY_KOTLIN_LSP") else {
        eprintln!("skipping Kotlin LSP overload differential: set KRUSTY_KOTLIN_LSP");
        return;
    };
    let _official_guard = OFFICIAL_DIFFERENTIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let project = TempProject::new("overload-diagnostic-differential");
    let root = project.path();
    let source_root = root.join("src/main/kotlin");
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::write(
        root.join("settings.gradle"),
        "rootProject.name = 'krusty-lsp-overload-diff'\n",
    )
    .unwrap();
    std::fs::write(
        root.join("build.gradle"),
        format!(
            "plugins {{ id 'org.jetbrains.kotlin.jvm' version '{}' }}\n\
             repositories {{ mavenCentral() }}\n",
            reference_kotlin_version()
        ),
    )
    .unwrap();
    std::fs::write(
        source_root.join("OtherOne.kt"),
        "fun namedPair(left: Int, right: String): String = right\n",
    )
    .unwrap();
    std::fs::write(
        source_root.join("OtherTwo.kt"),
        "fun namedPair(left: Int, right: String): String = right\n",
    )
    .unwrap();
    let source = "fun namedPair(left: Int, right: String): Int = left\n\
                  fun missingNamedArgument(): Int = namedPair(left = 1)\n";
    let source_path = source_root.join("Target.kt");
    std::fs::write(&source_path, source).unwrap();
    let root_uri = format!("file://{}", root.display());
    let uri = format!("file://{}", source_path.display());

    let mut reference = LspProcess::spawn(&kotlin_lsp, &["--stdio"]);
    reference.initialize(&root_uri);
    let expected = normalized_diagnostics(reference.diagnostics_at_least(&uri, source, 3));
    assert_eq!(expected.len(), 3, "official Kotlin LSP diagnostics");
    drop(reference);

    let mut krusty = LspProcess::spawn(env!("CARGO_BIN_EXE_krusty-lsp"), &["--stdio", "-no-jdk"]);
    krusty.initialize(&root_uri);
    let actual = normalized_diagnostics(krusty.diagnostics_allow_empty(&uri, source));
    assert_eq!(actual, expected);
}

#[test]
fn unused_extension_receiver_inspections_match_official_kotlin_lsp() {
    let Ok(kotlin_lsp) = std::env::var("KRUSTY_KOTLIN_LSP") else {
        eprintln!("skipping Kotlin LSP unused-receiver differential: set KRUSTY_KOTLIN_LSP");
        return;
    };
    let _official_guard = OFFICIAL_DIFFERENTIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let project = TempProject::new("unused-receiver-diagnostic-differential");
    let root = project.path();
    let source_root = root.join("src/main/kotlin");
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::write(
        root.join("settings.gradle"),
        "rootProject.name = 'krusty-lsp-unused-receiver-diff'\n",
    )
    .unwrap();
    std::fs::write(
        root.join("build.gradle"),
        format!(
            "plugins {{ id 'org.jetbrains.kotlin.jvm' version '{}' }}\n\
             repositories {{ mavenCentral() }}\n",
            reference_kotlin_version()
        ),
    )
    .unwrap();
    let source = "interface Parser\n\
                  interface JsonParser : Parser\n\
                  const val ANSWER = 42\n\
                  class Used(val length: Int) { fun member(): Int = length }\n\
                  class Mutable(var value: Int)\n\
                  class Outer { class Inner<T> }\n\
                  class Container(val answer: Int) {\n\
                    class Nested\n\
                    fun helper(): Int = 1\n\
                    fun String.dispatchPropertyOnly(): Int = answer\n\
                    fun String.dispatchCallOnly(): Int = helper()\n\
                    fun String.dispatchClassifierOnly(): Any = Nested()\n\
                  }\n\
                  class Collision(val length: Int) {\n\
                    fun String.extensionReceiverWins(): Int = length\n\
                    fun String.labelledDispatchOnly(): Int = this@Collision.length\n\
                  }\n\
                  fun <T, R> applyValue(value: T, block: (T) -> R): R = block(value)\n\
                  fun <T, R> T.scopeValue(block: T.() -> R): R = block()\n\
                  fun JsonParser.decode(source: String): Any = source\n\
                  fun Parser.decode(source: String): Any = source\n\
                  fun Parser.decode(value: Int): Any = value\n\
                  fun String.topLevelPropertyOnly(): Int = ANSWER\n\
                  fun String.explicitUse(): Int = this.length\n\
                  fun String.implicitPropertyUse(): Int = length\n\
                  fun String.implicitCallUse(): String = trim()\n\
                  fun Used.implicitCallableUse(): () -> Int = ::member\n\
                  fun Used.explicitCallableUse(): () -> Int = this::member\n\
                  fun Used.explicitPropertyCallableUse() = this::length\n\
                  fun String.boundProperty() = this::length\n\
                  fun String.implicitMethod(): () -> String = ::trim\n\
                  fun String.implicitLambdaParameterOnly(): Int = applyValue(1) { it }\n\
                  fun String.catchParameterOnly(): Int = try { 1 } catch (error: Throwable) { error.hashCode() }\n\
                  fun String.nestedReceiverOnly(): Int = Used(1).run { length }\n\
                  fun String.labelledOuterUse(): Int = 1.scopeValue { this@labelledOuterUse.length }\n\
                  fun Mutable.assignmentUse() { value = 1 }\n\
                  fun Mutable.incrementUse() { value++ }\n\
                  context(s: String) fun consumeContext(): Int = s.length\n\
                  fun String.contextArgumentUse(): Int = consumeContext()\n\
                  fun localExtensions() {\n\
                    fun String.localUnused(): Int = 1\n\
                    fun String.localUsed(): Int = length\n\
                  }\n\
                  class Extensions {\n\
                    fun String.memberUnused(value: Int): Int = value\n\
                    fun String.memberUsed(): Int = length\n\
                    companion object {\n\
                      fun String.companionUnused(value: Int): Int = value\n\
                      fun String.companionUsed(): Int = length\n\
                    }\n\
                  }\n\
                  val String.unusedProperty: Int get() = 1\n\
                  val String.usedProperty: Int get() = length\n\
                  fun Outer.Inner<String>.dottedFunction(): Int = 1\n\
                  val Outer.Inner<String>.dottedProperty: Int get() = 1\n\
                  fun (() -> Unit).parenthesizedFunction(): Int = 1\n\
                  val (() -> Unit).parenthesizedProperty: Int get() = 1\n";
    let source_path = source_root.join("UnusedReceiver.kt");
    std::fs::write(&source_path, source).unwrap();
    let root_uri = format!("file://{}", root.display());
    let uri = format!("file://{}", source_path.display());

    let mut reference = LspProcess::spawn(&kotlin_lsp, &["--stdio"]);
    reference.initialize(&root_uri);
    reference.open_document(&uri, source);
    let deadline = Instant::now() + ANALYSIS_TIMEOUT;
    let expected = loop {
        let diagnostics = normalized_diagnostics(reference.pull_diagnostics(&uri))
            .into_iter()
            .filter(|diagnostic| {
                diagnostic.get("message").and_then(Value::as_str)
                    == Some("Receiver parameter is never used")
            })
            .collect::<Vec<_>>();
        if diagnostics.len() == 19 {
            break diagnostics;
        }
        assert!(
            Instant::now() < deadline,
            "official Kotlin LSP did not stabilize at nineteen unused-receiver diagnostics: {diagnostics:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    drop(reference);

    let mut krusty = LspProcess::spawn(env!("CARGO_BIN_EXE_krusty-lsp"), &["--stdio", "-no-jdk"]);
    krusty.initialize(&root_uri);
    let actual = normalized_diagnostics(krusty.diagnostics_allow_empty(&uri, source))
        .into_iter()
        .filter(|diagnostic| {
            diagnostic.get("message").and_then(Value::as_str)
                == Some("Receiver parameter is never used")
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn implementation_locations_match_official_kotlin_lsp_exactly() {
    let Ok(kotlin_lsp) = std::env::var("KRUSTY_KOTLIN_LSP") else {
        eprintln!("skipping Kotlin LSP implementation differential: set KRUSTY_KOTLIN_LSP");
        return;
    };
    let _official_guard = OFFICIAL_DIFFERENTIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let project = TempProject::new("differential");
    let root = project.path();
    let source_root = root.join("src/main/kotlin");
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::write(
        root.join("settings.gradle"),
        "rootProject.name = 'krusty-lsp-implementation-diff'\n",
    )
    .unwrap();
    std::fs::write(
        root.join("build.gradle"),
        format!(
            "plugins {{ id 'org.jetbrains.kotlin.jvm' version '{}' }}\n\
             repositories {{ mavenCentral() }}\n",
            reference_kotlin_version()
        ),
    )
    .unwrap();

    let contract = "package implementationparity\n\
        interface ImplementationContract<T> {\n\
        \u{20}\u{20}fun route(value: T): String\n\
        \u{20}\u{20}fun pick(value: Int): String\n\
        \u{20}\u{20}fun pick(value: String): String\n\
        }\n\
        open class ImplementationBase<T>(ignored: Any? = null) : ImplementationContract<T> {\n\
        \u{20}\u{20}override fun route(value: T): String = value.toString()\n\
        \u{20}\u{20}override fun pick(value: Int): String = value.toString()\n\
        \u{20}\u{20}override fun pick(value: String): String = value\n\
        }\n";
    let leaf = "package implementationparity\n\
        class ImplementationLeaf : ImplementationBase<String>(ImplementationBase<Int>()) {\n\
        \u{20}\u{20}override fun route(value: String): String = value\n\
        \u{20}\u{20}override fun pick(value: Int): String = value.toString()\n\
        }\n";
    let usage = "package implementationparity\n\
        fun use(value: ImplementationContract<String>): String {\n\
        \u{20}\u{20}val emoji = \"😀\"; return value.route(\"x\") + value.pick(1)\n\
        }\n";
    let files = [
        ("ImplementationContract.kt", contract),
        ("ImplementationLeaf.kt", leaf),
        ("ImplementationUse.kt", usage),
    ];
    let warmup = "package implementationparity\nfun warmup(): String = 1\n";
    std::fs::write(source_root.join("Warmup.kt"), warmup).unwrap();
    for (name, source) in files {
        std::fs::write(source_root.join(name), source).unwrap();
    }
    let root_uri = format!("file://{}", root.display());
    let contract_uri = format!(
        "file://{}",
        source_root.join("ImplementationContract.kt").display()
    );
    let leaf_uri = format!(
        "file://{}",
        source_root.join("ImplementationLeaf.kt").display()
    );
    let use_uri = format!(
        "file://{}",
        source_root.join("ImplementationUse.kt").display()
    );
    let positions = [
        (
            "interface generic method",
            contract_uri.as_str(),
            position_after_marker(contract, "fun rou"),
        ),
        (
            "base generic method",
            contract_uri.as_str(),
            position_after_marker(contract, "override fun rou"),
        ),
        (
            "selected integer overload",
            contract_uri.as_str(),
            position_after_marker(contract, "fun pi"),
        ),
        (
            "generic call after emoji",
            use_uri.as_str(),
            position_after_marker(usage, "value.rou"),
        ),
        (
            "selected overload call after emoji",
            use_uri.as_str(),
            position_after_marker(usage, "value.pi"),
        ),
    ];

    let mut reference = LspProcess::spawn(&kotlin_lsp, &["--stdio"]);
    reference.initialize(&root_uri);
    let warmup_uri = format!("file://{}", source_root.join("Warmup.kt").display());
    assert!(!reference.diagnostics(&warmup_uri, warmup).is_empty());
    for (name, source) in files {
        let uri = format!("file://{}", source_root.join(name).display());
        reference.open_document(&uri, source);
    }
    let expected = positions
        .iter()
        .map(|(name, uri, (line, character))| {
            let result = reference.implementation(uri, *line, *character);
            assert!(
                result
                    .as_array()
                    .is_some_and(|locations| !locations.is_empty()),
                "official Kotlin LSP returned no implementations for {name}: {result}"
            );
            json!({"case": name, "result": result})
        })
        .collect::<Vec<_>>();
    let expected_leaf = reference.implementation(
        &leaf_uri,
        position_after_marker(leaf, "class ImplementationLe").0,
        position_after_marker(leaf, "class ImplementationLe").1,
    );
    assert!(
        expected_leaf.is_null(),
        "official Kotlin LSP unexpectedly returned an implementation for the leaf: {expected_leaf}"
    );
    drop(reference);

    let mut krusty = LspProcess::spawn(env!("CARGO_BIN_EXE_krusty-lsp"), &["--stdio", "-no-jdk"]);
    krusty.initialize(&root_uri);
    for (name, source) in files {
        let uri = format!("file://{}", source_root.join(name).display());
        krusty.open_document(&uri, source);
    }
    let actual = positions
        .iter()
        .map(|(name, uri, (line, character))| {
            json!({
                "case": name,
                "result": krusty.implementation(uri, *line, *character)
            })
        })
        .collect::<Vec<_>>();
    let leaf_position = position_after_marker(leaf, "class ImplementationLe");
    let actual_leaf = krusty.implementation(&leaf_uri, leaf_position.0, leaf_position.1);

    assert_eq!(actual, expected, "implementation location mismatches");
    assert_eq!(
        actual_leaf, expected_leaf,
        "negative implementation mismatch"
    );
}

#[test]
fn diagnostics_tokens_navigation_hovers_completions_and_symbols_match_official_kotlin_lsp() {
    let Ok(kotlin_lsp) = std::env::var("KRUSTY_KOTLIN_LSP") else {
        eprintln!("skipping Kotlin LSP differential: set KRUSTY_KOTLIN_LSP");
        return;
    };
    let _official_guard = OFFICIAL_DIFFERENTIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let project = TempProject::new("differential");
    let root = project.path();
    let source_root = root.join("src/main/kotlin");
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::write(
        root.join("settings.gradle"),
        "rootProject.name = 'krusty-lsp-diff'\n",
    )
    .unwrap();
    std::fs::write(
        root.join("build.gradle"),
        format!(
            "plugins {{ id 'org.jetbrains.kotlin.jvm' version '{}' }}\n\
             repositories {{ mavenCentral() }}\n",
            reference_kotlin_version()
        ),
    )
    .unwrap();
    let diagnostic_cases = [
        ("ReturnType.kt", "fun returnMismatch(): String = 1\n"),
        (
            "Unresolved.kt",
            "fun unresolvedValue(): Int = missingValue\n",
        ),
        (
            "ArgumentType.kt",
            "fun needsInt(value: Int): Int = value\n\
             fun argumentMismatch(): Int = needsInt(\"wrong\")\n",
        ),
        (
            "NullableMemberCall.kt",
            "fun nullableMemberCall(value: String?): String = value.substring(1)\n",
        ),
        (
            "SpacedNullableMemberCall.kt",
            "fun spacedNullableMemberCall(value: String?): String = value. /* gap */ substring(1)\n",
        ),
        (
            "NullableCallableProperty.kt",
            "class CallableHolder(val block: () -> Int)\n\
             fun nullableCallableProperty(value: CallableHolder?): Int = value.block()\n",
        ),
        (
            "NonNullGenericExtension.kt",
            "fun <T : Any> T.nonNullGeneric(): Int = hashCode()\n\
             fun nonNullGenericExtension(value: String?): Int = value.nonNullGeneric()\n",
        ),
        (
            "ConditionType.kt",
            "fun conditionMismatch(): Int { if (1) return 1; return 0 }\n",
        ),
        (
            "NullableReturnType.kt",
            "fun nullableReturnMismatch(): String = null\n",
        ),
        (
            "TooManyArguments.kt",
            "fun exact(value: Int): Int = value\n\
             fun tooManyArguments(): Int = exact(1, 2)\n",
        ),
        (
            "MissingArgument.kt",
            "fun pair(left: Int, right: String): Int = left\n\
             fun missingArgument(): Int = pair(1)\n",
        ),
        (
            "MissingNamedArgument.kt",
            "fun namedPair(left: Int, right: String): Int = left\n\
             fun missingNamedArgument(): Int = namedPair(left = 1)\n",
        ),
        (
            "MissingMemberArgument.kt",
            "class PairHolder { fun pair(left: Int, right: String): Int = left }\n\
             fun missingMemberArgument(holder: PairHolder): Int = holder.pair(1)\n",
        ),
        (
            "MissingConstructorArgument.kt",
            "class RequiredPair(val left: Int, val right: String)\n\
             fun missingConstructorArgument(): RequiredPair = RequiredPair(1)\n",
        ),
        (
            "UnknownNamedArgument.kt",
            "fun namedPair(left: Int, right: String): String = right\n\
             fun unknownNamedArgument(): String = namedPair(left = 1, unknown = 2, right = \"ok\")\n",
        ),
        (
            "DuplicateNamedArgument.kt",
            "fun namedPair(left: Int, right: String): String = right\n\
             fun duplicateNamedArgument(): String = namedPair(left = 1, left = 2, right = \"ok\")\n",
        ),
        (
            "ValReassignment.kt",
            "fun reassignValue(): Int {\n\
             \u{20}\u{20}val value = 1\n\
             \u{20}\u{20}value = 2\n\
             \u{20}\u{20}return value\n\
             }\n",
        ),
        (
            "MemberValReassignment.kt",
            "class ImmutableHolder(val value: Int)\n\
             fun reassignMember(holder: ImmutableHolder) { holder.value = 2 }\n",
        ),
        (
            "ValIncrement.kt",
            "fun incrementValue(): Int {\n\
             \u{20}\u{20}val value = 1\n\
             \u{20}\u{20}value++\n\
             \u{20}\u{20}return value\n\
             }\n",
        ),
        (
            "NonOperatorUnary.kt",
            "class NotAnOperator { fun unaryMinus(): NotAnOperator = this }\n\
             fun invalidUnary(): NotAnOperator = -NotAnOperator()\n",
        ),
        (
            "NonOperatorIncrement.kt",
            "class NotIncrementable { fun inc(): NotIncrementable = this }\n\
             fun invalidIncrement(input: NotIncrementable) { var value = input; value++ }\n",
        ),
    ];
    let clean_diagnostic_cases = [
        (
            "ContextArray.kt",
            "fun emptyStrings(): Array<String> = arrayOf()\n",
        ),
        (
            "StatementWhen.kt",
            "fun consumeUnit(block: () -> Unit) { block() }\n\
             fun statementWhen(value: Int) {\n\
             \u{20}\u{20}when (value) { 1 -> println(value) }\n\
             \u{20}\u{20}consumeUnit { when (value) { 2 -> println(value) } }\n\
             }\n",
        ),
        (
            "ExhaustiveNullableWhen.kt",
            "enum class CompleteNullableState { READY, DONE }\n\
             fun completeNullableWhen(value: CompleteNullableState?): Int = when (value) { CompleteNullableState.READY -> 1; CompleteNullableState.DONE -> 2; null -> 0 }\n",
        ),
        (
            "ValidNullableCalls.kt",
            "fun String?.nullableExtension(): Int = this?.length ?: 0\n\
             fun validNullableCalls(value: String?): Int {\n\
             \u{20}\u{20}val extension = value.nullableExtension()\n\
             \u{20}\u{20}val safe = value?.substring(1)\n\
             \u{20}\u{20}val asserted = value!!.substring(1)\n\
             \u{20}\u{20}return extension + (safe?.length ?: 0) + asserted.length\n\
             }\n",
        ),
        (
            "UnboundedGenericNullableReceiver.kt",
            "inline fun <T> T.acceptsNullable(block: () -> Unit): Boolean { block(); return this == null }\n\
             fun unboundedGenericNullableReceiver(value: String?): Boolean = value.acceptsNullable { }\n",
        ),
        (
            "BoundExtensionOverloadReference.kt",
            "interface Parser\n\
             interface JsonParser : Parser\n\
             fun JsonParser.decode(source: String): Any = source\n\
             fun Parser.decode(source: String): Any = source\n\
             fun Parser.decode(value: Int): Any = value\n\
             fun consume(decode: (String) -> Any) {}\n\
             fun validBoundReference(parser: JsonParser) { consume(parser::decode) }\n",
        ),
        (
            "TopLevelOverloadReference.kt",
            "fun select(value: Int, marker: Any): Int = value\n\
             fun select(value: Any, marker: Int): Int = marker\n\
             fun validTopLevelReference(): (Int, Any) -> Unit = ::select\n",
        ),
        (
            "GenericExpectedTypeAdaptedReference.kt",
            "fun adaptedTarget(x: String, y: Char = 'K'): String = x + y\n\
             fun <T, U> applyAdapted(block: (T) -> U, value: T): U = block(value)\n\
             fun validGenericAdaptedReference(): String = applyAdapted(::adaptedTarget, \"O\")\n",
        ),
    ];
    let token_cases = [
        (
            "BasicTokens.kt",
            "data class User(val name: String)\n\
             fun greet(user: User): String = user.name\n",
        ),
        (
            "MemberTokens.kt",
            "enum class Shade { RED }\n\
             class Holder(var mutable: Int, val fixed: Int)\n\
             fun paint(holder: Holder): Shade {\n\
                 holder.mutable = holder.fixed\n\
                 return Shade.RED\n\
             }\n",
        ),
        (
            "AdvancedTokens.kt",
            "package tokenparity\n\
             @Deprecated(\"old\") data class Record(val value: Int)\n\
             interface Named { fun name(): String }\n\
             object Registry { var current: Record? = null }\n\
             enum class State { READY }\n\
             typealias Alias = Record\n\
             operator fun Record.plus(other: Record): Record = this\n\
             fun use(input: Alias): Int {\n\
                 Registry.current = input\n\
                 return State.READY.ordinal + input.value\n\
             }\n",
        ),
        (
            "EnumInheritedMemberTokens.kt",
            "enum class Direction { NORTH }\n\
             fun describe(direction: Direction): String =\n\
                 direction.name + Direction.NORTH.ordinal\n",
        ),
        (
            "LabelTokens.kt",
            "fun sumPositive(values: IntArray): Int {\n\
                 var result = 0\n\
                 outer@ for (value in values) {\n\
                     if (value < 0) continue@outer\n\
                     if (value == 0) break@outer\n\
                     result += value\n\
                 }\n\
                 return result\n\
             }\n\
             fun pick(value: Int): Int = value.let {\n\
                 if (it > 0) return@let it\n\
                 return 0\n\
             }\n",
        ),
        (
            "ReceiverLabelTokens.kt",
            "interface LabelBase { fun value(): Int = 1 }\n\
             class LabelOuter : LabelBase {\n\
                 override fun value(): Int = super<LabelBase>@LabelOuter.value()\n\
                 fun self(): LabelOuter = noreturn@ this\n\
             }\n",
        ),
        (
            "UserCompoundOperatorTokens.kt",
            "class Accumulator {\n\
                 operator fun plusAssign(value: Int) {}\n\
             }\n\
             operator fun Accumulator.minusAssign(value: Int) {}\n\
             fun use(accumulator: Accumulator) {\n\
                 accumulator += 1\n\
                 accumulator -= 1\n\
             }\n",
        ),
        (
            "UnaryOperatorTokens.kt",
            "class Counter(var value: Int) {\n\
                 operator fun unaryMinus(): Counter = this\n\
                 operator fun inc(): Counter = this\n\
             }\n\
             class Marker\n\
             @Deprecated(\"old\") class OldMarker\n\
             operator fun Marker.not(): Marker = this\n\
             fun use(input: Int, counter: Counter): Int {\n\
                 var current = counter\n\
                 val negative = -input\n\
                 val inverted = !false\n\
                 val userNegative = -current\n\
                 current++\n\
                 ++current\n\
                 val previous = current++\n\
                 val next = ++current\n\
                 val extensionInverted = !Marker()\n\
                 val old = OldMarker()\n\
                 return negative\n\
             }\n",
        ),
        (
            "IncDecStorageTokens.kt",
            "class StorageCounter { operator fun inc(): StorageCounter = this }\n\
             class StorageHolder(var value: StorageCounter)\n\
             class StorageCounters(var value: StorageCounter) {\n\
                 operator fun get(index: Int): StorageCounter = value\n\
                 operator fun set(index: Int, value: StorageCounter) { this.value = value }\n\
             }\n\
             fun useStorage(holder: StorageHolder, counters: StorageCounters) {\n\
                 holder.value++\n\
                 counters[0]++\n\
             }\n",
        ),
    ];
    let signature_help_source = "package signatureparity\n\
         fun combine(left: String, count: Int = 1): String = left.repeat(count)\n\
         fun combine(left: Int, right: Int): Int = left + right\n\
         fun <T> identity(value: T): T = value\n\
         fun <T> choose(first: T, second: T): T = first\n\
         fun unicode(π: String, count: Int = 1): String = π.repeat(count)\n\
         fun collect(vararg values: Int): Int = values.size\n\
         fun zero(): Int = 0\n\
         class Box(val name: String, val size: Int = 1) {\n\
         \u{20}\u{20}fun member(prefix: String, times: Int = 1): String = prefix.repeat(times)\n\
         }\n\
         class Choice {\n\
         \u{20}\u{20}constructor(value: String) {}\n\
         \u{20}\u{20}constructor(value: Int, count: Int = 1) {}\n\
         }\n\
         open class ConstructorBase\n\
         class ConstructorDerived : ConstructorBase()\n\
         class SubtypeChoice {\n\
         \u{20}\u{20}constructor(value: String) {}\n\
         \u{20}\u{20}constructor(value: ConstructorBase) {}\n\
         }\n\
         class SecondaryOnly { constructor(value: Int) {} }\n\
         class Holder<T>(val value: T)\n\
         fun <T> unwrap(holder: Holder<T>): T = holder.value\n\
         fun <T> coalesce(value: T?, fallback: T): T = value ?: fallback\n\
         fun advanced(maybe: Int?) {\n\
         \u{20}\u{20}val selected = Choice(1, 2)\n\
         \u{20}\u{20}val subtypeSelected = SubtypeChoice(ConstructorDerived())\n\
         \u{20}\u{20}val secondary = SecondaryOnly(1)\n\
         \u{20}\u{20}val nestedGeneric = unwrap(Holder<Int>(1))\n\
         \u{20}\u{20}val nullableIdentity = identity(maybe)\n\
         \u{20}\u{20}val nullableHolder = unwrap(Holder<Int?>(maybe))\n\
         \u{20}\u{20}val mergedNullable = choose(maybe, 1)\n\
         \u{20}\u{20}val mixed = choose(1, \"x\")\n\
         \u{20}\u{20}val mixedReversed = choose(\"x\", 1)\n\
         \u{20}\u{20}val nullableGeneric = coalesce(maybe, 1)\n\
         }\n\
         fun use(box: Box): String {\n\
         \u{20}\u{20}val top = combine(\"top\", 2)\n\
         \u{20}\u{20}val member = box.member(\"member\", 3)\n\
         \u{20}\u{20}val constructed = Box(\"box\", 4)\n\
         \u{20}\u{20}val integer = combine(1, 2)\n\
         \u{20}\u{20}val named = combine(count = 2, left = \"named\")\n\
         \u{20}\u{20}val nested = combine(combine(\"inner\", 1), 2)\n\
         \u{20}\u{20}val generic = identity(1)\n\
         \u{20}\u{20}val nonAscii = unicode(\"π\", 2)\n\
         \u{20}\u{20}val variadic = collect(1, 2, 3)\n\
         \u{20}\u{20}val empty = zero()\n\
         \u{20}\u{20}fun local(value: String, count: Int = 1): String = value.repeat(count)\n\
         \u{20}\u{20}val localResult = local(\"local\", 2)\n\
         \u{20}\u{20}return top + member + constructed.name + integer + named + nested + generic + nonAscii + variadic + empty + localResult\n\
         }\n";
    let incomplete_signature_help_source =
        "fun combine(left: String, count: Int = 1): String = left\n\
         fun use(): String {\n\
         \u{20}\u{20}return combine(\"x\", \n\
         }\n";
    let rename_unicode_source =
        "fun unicodeRename(): Int { val target = \"😀\"; return target.length }\n";
    let backticked_source = "fun `odd name`(): Int = 1\n\
         fun useOdd(): Int = `odd name`()\n\
         fun parameterHover(`odd param`: Int): Int = `odd param`\n";
    let type_definition_target_source = "package typedefparity\n\
         import kotlin.reflect.KProperty\n\
         open class TypeParityBase\n\
         class TypeParityDerived : TypeParityBase()\n\
         data class TypeParityHolder(val item: TypeParityDerived)\n\
         class TypeParityDelegate {\n\
         \u{20}\u{20}operator fun getValue(thisRef: Any?, property: KProperty<*>): TypeParityDerived = TypeParityDerived()\n\
         }\n\
         class TypeParityFailure : Exception()\n\
         val typeParityExplicitOrdinary: TypeParityDerived = TypeParityDerived()\n\
         val typeParityInferredOrdinary = TypeParityDerived()\n\
         class TypeParityProperties {\n\
         \u{20}\u{20}val typeParityExplicitMember: TypeParityDerived = TypeParityDerived()\n\
         \u{20}\u{20}val typeParityInferredMember = TypeParityDerived()\n\
         }\n";
    let type_definition_use_source = "package typedefparity\n\
         fun typeParityUse(input: TypeParityDerived): TypeParityBase {\n\
         \u{20}\u{20}val inferred = input\n\
         \u{20}\u{20}val nullable: TypeParityDerived? = inferred\n\
         \u{20}\u{20}val copied = nullable\n\
         \u{20}\u{20}val holder = TypeParityHolder(inferred)\n\
         \u{20}\u{20}lateinit var delayed: TypeParityDerived\n\
         \u{20}\u{20}val delegated by TypeParityDelegate()\n\
         \u{20}\u{20}val (destructured) = TypeParityHolder(inferred)\n\
         \u{20}\u{20}for (element in arrayOf(inferred)) { element }\n\
         \u{20}\u{20}val mapper = { lambdaInput: TypeParityDerived -> lambdaInput }\n\
         \u{20}\u{20}try { inferred } catch (failure: TypeParityFailure) { inferred }\n\
         \u{20}\u{20}val emoji = \"😀\"; return holder.item\n\
         }\n\
         fun <T : TypeParityBase> typeParityGeneric(input: T) {\n\
         \u{20}\u{20}val explicitGeneric: T = input\n\
         \u{20}\u{20}val copiedGeneric = input\n\
         }\n\
         fun typeParitySmart(value: TypeParityBase): TypeParityDerived? {\n\
         \u{20}\u{20}if (value is TypeParityDerived) return value\n\
         \u{20}\u{20}return null\n\
         }\n";
    let definition_files = [
        (
            "DefinitionTarget.kt",
            "package demo\nfun answer(): Int = 42\n",
        ),
        (
            "DefinitionUse.kt",
            "package demo\nfun use(): Int = answer()\n",
        ),
        (
            "Locals.kt",
            "fun local(): Int {\n\
             \u{20}\u{20}\u{20}\u{20}val answer = 40\n\
             \u{20}\u{20}\u{20}\u{20}fun nested(): Int = 2\n\
             \u{20}\u{20}\u{20}\u{20}return answer + nested()\n\
             }\n",
        ),
        (
            "LocalKinds.kt",
            "fun useLocalKinds(): Int {\n\
             \u{20}\u{20}fun size(): Int = 2\n\
             \u{20}\u{20}val size: Int = 1\n\
             \u{20}\u{20}return size + size()\n\
             }\n",
        ),
        (
            "TopKinds.kt",
            "package topkinds\nval size: Int = 1\nfun size(): Int = 2\n",
        ),
        (
            "TopKindsUse.kt",
            "package topkinds\nfun useTopKinds(): Int = size + size()\n",
        ),
        (
            "ReceiverlessExtension.kt",
            "fun String.ext(): Int = 1\nfun useReceiverless(): Int = ext()\n",
        ),
        (
            "HoverAdvanced.kt",
            "package hoveradvanced\n\
             open class GenericBox<T>(val value: T) {\n\
             \u{20}\u{20}open var mutable: String? = null\n\
             \u{20}\u{20}protected open fun combine(vararg values: Int): String? = null\n\
             }\n\
             interface Named {\n\
             \u{20}\u{20}fun label(prefix: String = \"x\"): String\n\
             }\n\
             suspend fun <T : Any> transform(value: T): T = value\n\
             fun inferred() = 42\n\
             fun use(box: GenericBox<Int>): String? = box.mutable\n",
        ),
        (
            "HoverInference.kt",
            "package hoverinference\n\
             import kotlin.reflect.KProperty\n\
             open class Base { open fun pick(): Int = 1 }\n\
             class Child : Base() { override fun pick(): Int = 2 }\n\
             val getterOnly get() = 42\n\
             class Delegate { operator fun getValue(t: Any?, p: KProperty<*>): Int = 1 }\n\
             fun locals(): Int {\n\
             \u{20}\u{20}fun local() = 42\n\
             \u{20}\u{20}val inferred by Delegate()\n\
             \u{20}\u{20}return local() + inferred\n\
             }\n\
             fun loop(values: IntArray): Int {\n\
             \u{20}\u{20}for (value in values) return value\n\
             \u{20}\u{20}return 0\n\
             }\n\
             fun lambdaHover(): Int {\n\
             \u{20}\u{20}val fn: (Int) -> Int = { value -> value + 1 }\n\
             \u{20}\u{20}return fn(1)\n\
             }\n\
             fun catchHover(): Int = try { 1 } catch (error: Throwable) { 2 }\n\
             val blockGetter get() { return 42 }\n\
             val topDelegated by Delegate()\n\
             class DelegatedHolder { val memberDelegated by Delegate() }\n",
        ),
        (
            "HoverModifiers.kt",
            "package hovermodifiers\n\
             fun interface Action { fun run(): Int }\n\
             @JvmInline value class Meter(val value: Int)\n\
             class Outer { inner class Inner { fun answer(): Int = 1 } }\n\
             enum class Code(val wire: Int) { OK(1) }\n\
             annotation class Label(val text: String)\n\
             const val ANSWER: Int = 42\n\
             lateinit var text: String\n\
             open class Parent { open val item: Int = 1; open fun pick(): Int = 1 }\n\
             class Child : Parent() { override val item: Int = 2; override fun pick(): Int = 2 }\n\
             inline fun inlined(): Int = 1\n\
             tailrec fun tail(n: Int): Int = if (n == 0) 0 else tail(n - 1)\n\
             operator fun Meter.plus(other: Meter): Meter = Meter(value + other.value)\n\
             fun nestedUse(outer: Outer): Int = outer.Inner().answer()\n",
        ),
        (
            "HoverReview.kt",
            "package hoverreview\n\
             data class Parts(val number: Int, val text: String)\n\
             class Outer { class Inner }\n\
             fun nestedLocal(): Int {\n\
             \u{20}\u{20}val nested = Outer.Inner()\n\
             \u{20}\u{20}return nested.hashCode()\n\
             }\n\
             fun destructure(parts: Parts): Int {\n\
             \u{20}\u{20}val (number, text) = parts\n\
             \u{20}\u{20}return number\n\
             }\n\
             class Meter(val value: Int)\n\
             operator\n\
             fun Meter.plus(other: Meter): Meter = this\n\
             open class Parent { open val item: Int = 1 }\n\
             class Child : Parent() {\n\
             \u{20}\u{20}override\n\
             \u{20}\u{20}val item: Int = 2\n\
             }\n",
        ),
        (
            "NestedDefinitions.kt",
            "package nesteddefinitions\n\
             class Outer { inner class Inner { fun answer(): Int = 1 } }\n",
        ),
        (
            "NestedUse.kt",
            "package nesteduse\n\
             import nesteddefinitions.Outer\n\
             import nesteddefinitions.Outer.Inner\n\
             fun imported(value: Inner): Int = value.answer()\n\
             fun constructed(outer: Outer): Int = outer.Inner().answer()\n\
             fun qualified(value: nesteddefinitions.Outer.Inner): Int = value.answer()\n",
        ),
        (
            "NestedOuterImportUse.kt",
            "package nestedouterimport\n\
             import nesteddefinitions.Outer\n\
             fun importedOuter(value: Outer.Inner): Outer.Inner = value\n",
        ),
        (
            "NestedOuterWildcardUse.kt",
            "package nestedouterwildcard\n\
             import nesteddefinitions.*\n\
             fun wildcardOuter(value: Outer.Inner): Outer.Inner = value\n",
        ),
        (
            "NestedEnclosingUse.kt",
            "package nestedenclosing\n\
             class Enclosing {\n\
             \u{20}\u{20}inner class Nested\n\
             \u{20}\u{20}fun echo(value: Nested): Nested = value\n\
             }\n",
        ),
        (
            "NestedCallableCollision.kt",
            "package nestedcollision\n\
             class Outer { inner class Item; fun Item(): Int = 1 }\n\
             fun use(outer: Outer): Int = outer.Item()\n",
        ),
        (
            "Overloads.kt",
            "fun select(value: Int): Int = value\n\
             fun select(value: String): Int = value.length\n\
             fun choose(): Int = select(1)\n",
        ),
        (
            "MemberOverloads.kt",
            "class Choices {\n\
             \u{20}\u{20}fun select(value: Int): Int = value\n\
             \u{20}\u{20}fun select(value: String): Int = value.length\n\
             }\n\
             fun chooseMember(c: Choices): Int = c.select(1)\n",
        ),
        (
            "Inherited.kt",
            "open class Base {\n\
             \u{20}\u{20}fun inherited(): Int = 1\n\
             \u{20}\u{20}val value: Int = 2\n\
             }\n\
             class Child : Base()\n\
             fun useInherited(c: Child): Int = c.inherited() + c.value\n",
        ),
        (
            "BodyProperty.kt",
            "class Body {\n\
             \u{20}\u{20}val value: Int = 1\n\
             \u{20}\u{20}fun get(): Int = value\n\
             }\n",
        ),
        ("Backticked.kt", backticked_source),
        ("RenameUnicode.kt", rename_unicode_source),
        (
            "MemberKinds.kt",
            "class Sized {\n\
             \u{20}\u{20}val size: Int = 1\n\
             \u{20}\u{20}fun size(): Int = 2\n\
             }\n\
             fun useKinds(c: Sized): Int = c.size() + c.size\n",
        ),
        (
            "MemberStaticness.kt",
            "class Mixed {\n\
             \u{20}\u{20}fun pick(): Int = 1\n\
             \u{20}\u{20}companion object {\n\
             \u{20}\u{20}\u{20}\u{20}fun pick(): Int = 2\n\
             \u{20}\u{20}}\n\
             }\n\
             fun useStaticness(m: Mixed): Int = m.pick() + Mixed.pick()\n",
        ),
        (
            "BacktickedMembers.kt",
            "class Weird(val `odd name`: Int)\n\
             fun useWeird(w: Weird): Int = w.`odd name`\n\
             enum class WeirdEnum { `odd entry` }\n\
             fun enumUse(): WeirdEnum = WeirdEnum.`odd entry`\n",
        ),
        (
            "Extension.kt",
            "class C\n\
             fun C.ext(x: Int): Int = x\n\
             fun useExtension(c: C): Int = c.ext(1)\n",
        ),
        (
            "ExtensionProperty.kt",
            "package extprop\n\
             class C\n\
             val C.ext: Int get() = 1\n\
             fun useProperty(c: C): Int = c.ext\n",
        ),
        (
            "GenericExtension.kt",
            "fun <T> T.identity(): T = this\n\
             fun useIdentity(): Int = 1.identity()\n",
        ),
        (
            "ObjectMembers.kt",
            "object Obj {\n\
             \u{20}\u{20}val prop: Int = 1\n\
             \u{20}\u{20}fun pick(): Int = prop\n\
             }\n\
             fun useObject(): Int = Obj.pick() + Obj.prop\n",
        ),
        (
            "SuperCall.kt",
            "package supercase\n\
             open class SuperBase {\n\
             \u{20}\u{20}open fun pick(value: Int): Int = value\n\
             \u{20}\u{20}open fun pick(value: String): Int = value.length\n\
             }\n\
             class SuperChild : SuperBase() {\n\
             \u{20}\u{20}override fun pick(value: Int): Int = value + 1\n\
             \u{20}\u{20}fun parent(): Int = super.pick(1)\n\
             }\n",
        ),
        (
            "InheritedSuperCall.kt",
            "package inheritedsuper\n\
             open class GrandUnique {\n\
             \u{20}\u{20}open fun pick(value: Int): Int = value\n\
             }\n\
             open class MiddleUnique : GrandUnique() {\n\
             \u{20}\u{20}open fun pick(value: String): Int = value.length\n\
             }\n\
             class ChildUnique : MiddleUnique() {\n\
             \u{20}\u{20}fun parent(): Int = super.pick(1)\n\
             }\n",
        ),
        (
            "ImportedUse.kt",
            "package use\nimport a.Item\nfun read(x: Item): Int = x.left\n",
        ),
        (
            "QualifiedUse.kt",
            "package use\nfun readQualified(x: a.Item): Int = x.left\n",
        ),
        (
            "QualifiedLocalUse.kt",
            "package use\nfun readLocal(seed: a.Item): Int {\n\
             \u{20}\u{20}val x: a.Item = seed\n\
             \u{20}\u{20}return x.left\n\
             }\n",
        ),
        (
            "CompanionScope.kt",
            "class MixedScope {\n\
             \u{20}\u{20}fun pick(): Int = 2\n\
             \u{20}\u{20}companion object {\n\
             \u{20}\u{20}\u{20}\u{20}fun pick(): Int = 1\n\
             \u{20}\u{20}\u{20}\u{20}fun call(): Int = pick()\n\
             \u{20}\u{20}}\n\
             }\n",
        ),
        ("Declaration.kt", "fun declaredAnswer(): Int = 42\n"),
        ("ImportedClass.kt", "package imports\nclass Imported\n"),
        (
            "ImportTerminal.kt",
            "package use\nimport imports.Imported\nfun imported(x: Imported): Imported = x\n",
        ),
        ("PackageA.kt", "package a\ndata class Item(val left: Int)\n"),
        (
            "PackageB.kt",
            "package b\ndata class Item(val right: Int)\n",
        ),
        (
            "PackageUse.kt",
            "package b\nfun useItem(item: Item): Int = item.right\n",
        ),
        (
            "CompletionParity.kt",
            "package completionparity\n\
             class KrustyParityBox(val krustyParityValue: Int) {\n\
             \u{20}\u{20}var krustyParityMutable: String = \"\"\n\
             \u{20}\u{20}fun krustyParityCall(arg: String): Int = arg.length\n\
             }\n\
             fun krustyParityTop(): Int = 1\n\
             fun krustyParityInferred() = 3\n\
             val krustyParityGlobal: Int = 2\n\
             fun completionUse(box: KrustyParityBox): Int {\n\
             \u{20}\u{20}val krustyParityLocal: Int = 1\n\
             \u{20}\u{20}fun krustyParityNested(): Int = 2\n\
             \u{20}\u{20}box.krustyParity\n\
             \u{20}\u{20}return krustyParity\n\
             }\n",
        ),
        (
            "IncrementalParity.kt",
            "fun before(): Int = 1\nfun use(): Int = before()\n",
        ),
        ("SignatureHelp.kt", signature_help_source),
        (
            "IncompleteSignatureHelp.kt",
            incomplete_signature_help_source,
        ),
        (
            "DocumentSymbols.kt",
            "package documentparity\n\
             val topValue: Int = 1\n\
             fun topFunction(arg: Int): Int = arg\n\
             class Box(val item: Int) {\n\
             \u{20}\u{20}var mutable: String = \"\"\n\
             \u{20}\u{20}fun member(value: Int): Int = value\n\
             }\n\
             enum class Shade { RED, BLUE }\n\
             interface Named { fun label(): String }\n\
             object Registry { val size: Int = 1; fun clear() {} }\n",
        ),
        (
            "DocumentSymbolsAdvanced.kt",
            "package documentsymboladvanced\n\
             @Deprecated(\"old\")\n\
             fun oldFunction(): Int = 1\n\
             @Deprecated(\n\
             \u{20}\u{20}\"old property\"\n\
             )\n\
             val oldProperty: Int = 1\n\
             class Advanced(seed: Int, val first: String = \"a,b\", var second: Int = 2)\n\
             class Outer {\n\
             \u{20}\u{20}class Inner(val nestedValue: Int) {\n\
             \u{20}\u{20}\u{20}\u{20}fun nestedMethod(): Int = nestedValue\n\
             \u{20}\u{20}}\n\
             }\n\
             fun `odd name`(): Int = 1\n",
        ),
        (
            "DocumentSymbolsScopes.kt",
            "package documentsymbolscopes\n\
             class Container {\n\
             \u{20}\u{20}companion object Factory {\n\
             \u{20}\u{20}\u{20}\u{20}val answer: Int = 42\n\
             \u{20}\u{20}\u{20}\u{20}fun create(): Container = Container()\n\
             \u{20}\u{20}}\n\
             \u{20}\u{20}fun outer(): Int {\n\
             \u{20}\u{20}\u{20}\u{20}val localValue: Int = 1\n\
             \u{20}\u{20}\u{20}\u{20}fun localFunction(): Int = localValue\n\
             \u{20}\u{20}\u{20}\u{20}class LocalClass(val localProperty: Int)\n\
             \u{20}\u{20}\u{20}\u{20}return localFunction()\n\
             \u{20}\u{20}}\n\
             }\n",
        ),
        (
            "DocumentSymbolsKinds.kt",
            "package documentsymbolkinds\n\
             annotation class Marker\n\
             data class Record(val value: Int)\n\
             @JvmInline\n\
             value class Token(val raw: Int)\n\
             fun interface Action { fun run(): Unit }\n\
             sealed class Base\n\
             typealias Alias = Record\n\
             @Deprecated(\n\
             \u{20}\u{20}\"old alias\"\n\
             )\n\
             typealias OldAlias = Record\n\
             class Injected @Deprecated(\"old constructor\") constructor(val injected: Int)\n\
             enum class Pair(val left: Int, val right: Int) { BOTH(1, 2) }\n\
             class Secondary {\n\
             \u{20}\u{20}constructor(value: Int) { println(value) }\n\
             }\n\
             class AnnotatedMembers {\n\
             \u{20}\u{20}@Deprecated(\"old secondary\")\n\
             \u{20}\u{20}constructor(value: Int) { println(value) }\n\
             \u{20}\u{20}@Deprecated(\"old companion\")\n\
             \u{20}\u{20}companion object {}\n\
             }\n\
             class DefaultCompanion {\n\
             \u{20}\u{20}companion object { val only: Int = 1 }\n\
             }\n",
        ),
        (
            "FoldingRanges.kt",
            "package foldingparity\n\
             import kotlin.collections.List\n\
             import kotlin.collections.Map\n\
             \n\
             /**\n\
             \u{20}* Documentation block.\n\
             \u{20}*/\n\
             class Box(\n\
             \u{20}\u{20}val value: Int,\n\
             ) {\n\
             \u{20}\u{20}/*\n\
             \u{20}\u{20} * Nested block comment.\n\
             \u{20}\u{20} */\n\
             \u{20}\u{20}fun choose(flag: Boolean): Int {\n\
             \u{20}\u{20}\u{20}\u{20}if (flag) {\n\
             \u{20}\u{20}\u{20}\u{20}\u{20}\u{20}return 1\n\
             \u{20}\u{20}\u{20}\u{20}}\n\
             \u{20}\u{20}\u{20}\u{20}return 2\n\
             \u{20}\u{20}}\n\
             }\n",
        ),
        (
            "FoldingRangesAdvanced.kt",
            "package foldingadvanced\n\
             \n\
             //region Utilities\n\
             // First line comment.\n\
             // Second line comment.\n\
             fun grouped(\n\
             \u{20}\u{20}first: Int,\n\
             \u{20}\u{20}second: Int,\n\
             ): Int = listOf(\n\
             \u{20}\u{20}first,\n\
             \u{20}\u{20}second,\n\
             ).map {\n\
             \u{20}\u{20}it + 1\n\
             }.sum()\n\
             //endregion\n\
             \n\
             val raw = \"\"\"\n\
             \u{20}\u{20}first\n\
             \u{20}\u{20}second\n\
             \"\"\".trimIndent()\n\
             \n\
             fun choose(value: Int): Int = when (value) {\n\
             \u{20}\u{20}0 -> {\n\
             \u{20}\u{20}\u{20}\u{20}1\n\
             \u{20}\u{20}}\n\
             \u{20}\u{20}else -> value\n\
             }\n\
             \n\
             fun escaped(\n\
             \u{20}\u{20}`)`: Int,\n\
             ): Int = listOf(\n\
             \u{20}\u{20}`)`,\n\
             \u{20}\u{20}1,\n\
             ).first()\n\
             \n\
             /* commented() */\n\
             fun commented(/* ) commented() */\n\
             \u{20}\u{20}value: String,\n\
             ): String = listOf(\n\
             \u{20}\u{20}value,\n\
             \u{20}\u{20}\"ok\",\n\
             ).first()\n\
             \n\
             fun rawHeader(value: String = \"\"\") rawHeader()\n\
             \"\"\",\n\
             ): String = listOf(\n\
             \u{20}\u{20}value,\n\
             \u{20}\u{20}\"ok\",\n\
             ).first()\n",
        ),
        ("TypeDefinitionTarget.kt", type_definition_target_source),
        ("TypeDefinitionUse.kt", type_definition_use_source),
    ];
    for (name, source) in diagnostic_cases
        .iter()
        .chain(&clean_diagnostic_cases)
        .chain(&token_cases)
        .chain(&definition_files)
    {
        std::fs::write(source_root.join(name), source).unwrap();
    }
    let root_uri = format!("file://{}", root.display());

    let mut reference = LspProcess::spawn(&kotlin_lsp, &["--stdio"]);
    let reference_legend = reference.initialize(&root_uri);
    let expected_diagnostics = diagnostic_cases
        .iter()
        .map(|(name, source)| {
            let uri = format!("file://{}", source_root.join(name).display());
            normalized_diagnostics(reference.diagnostics(&uri, source))
        })
        .collect::<Vec<_>>();
    let expected_clean_diagnostics = clean_diagnostic_cases
        .iter()
        .map(|(name, source)| {
            let uri = format!("file://{}", source_root.join(name).display());
            normalized_diagnostics(reference.diagnostics_allow_empty(&uri, source))
        })
        .collect::<Vec<_>>();
    let expected_tokens = token_cases
        .iter()
        .map(|(name, source)| {
            let uri = format!("file://{}", source_root.join(name).display());
            reference.semantic_tokens(&uri, source, &reference_legend)
        })
        .collect::<Vec<_>>();
    let basic_tokens_uri = format!("file://{}", source_root.join("BasicTokens.kt").display());
    let member_tokens_uri = format!("file://{}", source_root.join("MemberTokens.kt").display());
    for (name, source) in definition_files {
        let uri = format!("file://{}", source_root.join(name).display());
        reference.open_document(&uri, source);
    }
    let definition_target_uri = format!(
        "file://{}",
        source_root.join("DefinitionTarget.kt").display()
    );
    let definition_use_uri = format!("file://{}", source_root.join("DefinitionUse.kt").display());
    let locals_uri = format!("file://{}", source_root.join("Locals.kt").display());
    let local_kinds_uri = format!("file://{}", source_root.join("LocalKinds.kt").display());
    let top_kinds_use_uri = format!("file://{}", source_root.join("TopKindsUse.kt").display());
    let receiverless_extension_uri = format!(
        "file://{}",
        source_root.join("ReceiverlessExtension.kt").display()
    );
    let hover_advanced_uri = format!("file://{}", source_root.join("HoverAdvanced.kt").display());
    let hover_inference_uri = format!("file://{}", source_root.join("HoverInference.kt").display());
    let hover_modifiers_uri = format!("file://{}", source_root.join("HoverModifiers.kt").display());
    let hover_review_uri = format!("file://{}", source_root.join("HoverReview.kt").display());
    let nested_use_uri = format!("file://{}", source_root.join("NestedUse.kt").display());
    let nested_outer_import_use_uri = format!(
        "file://{}",
        source_root.join("NestedOuterImportUse.kt").display()
    );
    let nested_outer_wildcard_use_uri = format!(
        "file://{}",
        source_root.join("NestedOuterWildcardUse.kt").display()
    );
    let nested_enclosing_use_uri = format!(
        "file://{}",
        source_root.join("NestedEnclosingUse.kt").display()
    );
    let nested_callable_collision_uri = format!(
        "file://{}",
        source_root.join("NestedCallableCollision.kt").display()
    );
    let overloads_uri = format!("file://{}", source_root.join("Overloads.kt").display());
    let member_overloads_uri = format!(
        "file://{}",
        source_root.join("MemberOverloads.kt").display()
    );
    let inherited_uri = format!("file://{}", source_root.join("Inherited.kt").display());
    let body_property_uri = format!("file://{}", source_root.join("BodyProperty.kt").display());
    let backticked_uri = format!("file://{}", source_root.join("Backticked.kt").display());
    let rename_unicode_uri = format!("file://{}", source_root.join("RenameUnicode.kt").display());
    let member_kinds_uri = format!("file://{}", source_root.join("MemberKinds.kt").display());
    let member_staticness_uri = format!(
        "file://{}",
        source_root.join("MemberStaticness.kt").display()
    );
    let backticked_members_uri = format!(
        "file://{}",
        source_root.join("BacktickedMembers.kt").display()
    );
    let extension_uri = format!("file://{}", source_root.join("Extension.kt").display());
    let extension_property_uri = format!(
        "file://{}",
        source_root.join("ExtensionProperty.kt").display()
    );
    let generic_extension_uri = format!(
        "file://{}",
        source_root.join("GenericExtension.kt").display()
    );
    let object_members_uri = format!("file://{}", source_root.join("ObjectMembers.kt").display());
    let super_call_uri = format!("file://{}", source_root.join("SuperCall.kt").display());
    let inherited_super_call_uri = format!(
        "file://{}",
        source_root.join("InheritedSuperCall.kt").display()
    );
    let imported_use_uri = format!("file://{}", source_root.join("ImportedUse.kt").display());
    let qualified_use_uri = format!("file://{}", source_root.join("QualifiedUse.kt").display());
    let qualified_local_use_uri = format!(
        "file://{}",
        source_root.join("QualifiedLocalUse.kt").display()
    );
    let companion_scope_uri = format!("file://{}", source_root.join("CompanionScope.kt").display());
    let declaration_uri = format!("file://{}", source_root.join("Declaration.kt").display());
    let import_terminal_uri = format!("file://{}", source_root.join("ImportTerminal.kt").display());
    let package_use_uri = format!("file://{}", source_root.join("PackageUse.kt").display());
    let completion_parity_uri = format!(
        "file://{}",
        source_root.join("CompletionParity.kt").display()
    );
    let incremental_parity_uri = format!(
        "file://{}",
        source_root.join("IncrementalParity.kt").display()
    );
    let signature_help_uri = format!("file://{}", source_root.join("SignatureHelp.kt").display());
    let incomplete_signature_help_uri = format!(
        "file://{}",
        source_root.join("IncompleteSignatureHelp.kt").display()
    );
    let document_symbols_uri = format!(
        "file://{}",
        source_root.join("DocumentSymbols.kt").display()
    );
    let advanced_document_symbols_uri = format!(
        "file://{}",
        source_root.join("DocumentSymbolsAdvanced.kt").display()
    );
    let scoped_document_symbols_uri = format!(
        "file://{}",
        source_root.join("DocumentSymbolsScopes.kt").display()
    );
    let kind_document_symbols_uri = format!(
        "file://{}",
        source_root.join("DocumentSymbolsKinds.kt").display()
    );
    let folding_ranges_uri = format!("file://{}", source_root.join("FoldingRanges.kt").display());
    let advanced_folding_ranges_uri = format!(
        "file://{}",
        source_root.join("FoldingRangesAdvanced.kt").display()
    );
    let type_definition_use_uri = format!(
        "file://{}",
        source_root.join("TypeDefinitionUse.kt").display()
    );
    let type_definition_target_uri = format!(
        "file://{}",
        source_root.join("TypeDefinitionTarget.kt").display()
    );
    let definition_positions = [
        ("class reference", basic_tokens_uri.as_str(), 1, 17),
        ("parameter reference", basic_tokens_uri.as_str(), 1, 33),
        ("property reference", basic_tokens_uri.as_str(), 1, 38),
        (
            "cross-file function reference",
            definition_use_uri.as_str(),
            1,
            18,
        ),
        ("local variable reference", locals_uri.as_str(), 3, 12),
        ("local function reference", locals_uri.as_str(), 3, 21),
        ("same-name local value", local_kinds_uri.as_str(), 3, 9),
        ("same-name local function", local_kinds_uri.as_str(), 3, 16),
        (
            "same-name cross-file top-level value",
            top_kinds_use_uri.as_str(),
            1,
            26,
        ),
        (
            "same-name cross-file top-level function",
            top_kinds_use_uri.as_str(),
            1,
            33,
        ),
        ("resolved overload reference", overloads_uri.as_str(), 2, 21),
        (
            "resolved member overload reference",
            member_overloads_uri.as_str(),
            4,
            39,
        ),
        (
            "inherited function reference",
            inherited_uri.as_str(),
            5,
            37,
        ),
        (
            "inherited property reference",
            inherited_uri.as_str(),
            5,
            53,
        ),
        (
            "unqualified body property reference",
            body_property_uri.as_str(),
            2,
            20,
        ),
        (
            "backticked opening delimiter",
            backticked_uri.as_str(),
            1,
            20,
        ),
        (
            "zero-argument member function",
            member_kinds_uri.as_str(),
            4,
            33,
        ),
        (
            "same-name member property",
            member_kinds_uri.as_str(),
            4,
            44,
        ),
        (
            "instance member with companion namesake",
            member_staticness_uri.as_str(),
            6,
            39,
        ),
        (
            "companion member with instance namesake",
            member_staticness_uri.as_str(),
            6,
            54,
        ),
        (
            "backticked constructor property",
            backticked_members_uri.as_str(),
            1,
            32,
        ),
        (
            "backticked enum entry",
            backticked_members_uri.as_str(),
            3,
            37,
        ),
        ("source extension function", extension_uri.as_str(), 2, 33),
        (
            "source extension property",
            extension_property_uri.as_str(),
            3,
            32,
        ),
        (
            "generic source extension",
            generic_extension_uri.as_str(),
            1,
            28,
        ),
        ("object function member", object_members_uri.as_str(), 4, 28),
        ("object property member", object_members_uri.as_str(), 4, 41),
        ("selected super overload", super_call_uri.as_str(), 7, 29),
        (
            "inherited super overload past namesake",
            inherited_super_call_uri.as_str(),
            8,
            29,
        ),
        (
            "imported receiver property",
            imported_use_uri.as_str(),
            2,
            28,
        ),
        (
            "qualified receiver property",
            qualified_use_uri.as_str(),
            1,
            39,
        ),
        (
            "qualified local receiver property",
            qualified_local_use_uri.as_str(),
            3,
            12,
        ),
        (
            "unqualified companion call",
            companion_scope_uri.as_str(),
            4,
            23,
        ),
        ("declaration self target", declaration_uri.as_str(), 0, 5),
        (
            "import terminal target",
            import_terminal_uri.as_str(),
            1,
            16,
        ),
        (
            "same-package class reference",
            package_use_uri.as_str(),
            1,
            19,
        ),
        (
            "same-package property reference",
            package_use_uri.as_str(),
            1,
            37,
        ),
        (
            "selected member over same-named inner class",
            nested_callable_collision_uri.as_str(),
            2,
            36,
        ),
        (
            "nested type through imported outer class",
            nested_outer_import_use_uri.as_str(),
            2,
            32,
        ),
        (
            "nested type through wildcard-imported outer class",
            nested_outer_wildcard_use_uri.as_str(),
            2,
            32,
        ),
        (
            "simple nested type inside outer class",
            nested_enclosing_use_uri.as_str(),
            3,
            20,
        ),
    ];
    let expected_definitions = definition_positions
        .iter()
        .map(|(name, uri, line, character)| {
            let result = reference.definition(uri, *line, *character);
            assert!(
                !result.is_null(),
                "official Kotlin LSP returned null for definition case {name}"
            );
            json!({
                "case": name,
                "result": result
            })
        })
        .collect::<Vec<_>>();
    let negative_definition_positions = [(
        "receiverless extension call",
        receiverless_extension_uri.as_str(),
        1,
        30,
    )];
    let expected_negative_definitions = negative_definition_positions
        .iter()
        .map(|(name, uri, line, character)| {
            let result = reference.definition(uri, *line, *character);
            assert!(
                result.as_array().is_some_and(|locations| locations.is_empty()),
                "official Kotlin LSP unexpectedly resolved negative definition case {name}: {result}"
            );
            json!({
                "case": name,
                "result": result
            })
        })
        .collect::<Vec<_>>();
    let type_definition_positions = [
        (
            "class declaration self",
            type_definition_target_uri.as_str(),
            position_after_marker(type_definition_target_source, "class TypeParityDer"),
        ),
        (
            "constructor property declaration",
            type_definition_target_uri.as_str(),
            position_after_marker(type_definition_target_source, "val it"),
        ),
        (
            "explicit top-level property declaration",
            type_definition_target_uri.as_str(),
            position_after_marker(type_definition_target_source, "val typeParityExp"),
        ),
        (
            "inferred top-level property declaration",
            type_definition_target_uri.as_str(),
            position_after_marker(type_definition_target_source, "val typeParityInf"),
        ),
        (
            "explicit member property declaration",
            type_definition_target_uri.as_str(),
            position_after_marker(type_definition_target_source, "val typeParityExplicitMem"),
        ),
        (
            "inferred member property declaration",
            type_definition_target_uri.as_str(),
            position_after_marker(type_definition_target_source, "val typeParityInferredMem"),
        ),
        (
            "parameter declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "typeParityUse(inp"),
        ),
        (
            "explicit parameter type",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "input: TypeParityDer"),
        ),
        (
            "explicit return type",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "): TypeParityBa"),
        ),
        (
            "inferred local declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "val inf"),
        ),
        (
            "inferred local value",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "val inferred = inp"),
        ),
        (
            "nullable local declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "val null"),
        ),
        (
            "nullable local reference",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "val copied = null"),
        ),
        (
            "nullable inferred declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "val cop"),
        ),
        (
            "constructor result",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "val holder = TypeParityHol"),
        ),
        (
            "constructor inferred declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "val hol"),
        ),
        (
            "property result after emoji",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "return holder.it"),
        ),
        (
            "lateinit local declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "lateinit var dela"),
        ),
        (
            "delegated local declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "val deleg"),
        ),
        (
            "destructured local declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "val (destr"),
        ),
        (
            "for-each local declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "for (elem"),
        ),
        (
            "explicit lambda parameter declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "{ lambdaInp"),
        ),
        (
            "catch parameter declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "catch (fail"),
        ),
        (
            "smart-cast value",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "return val"),
        ),
    ];
    let expected_type_definitions = type_definition_positions
        .iter()
        .map(|(name, uri, (line, character))| {
            let result = reference.type_definition(uri, *line, *character);
            assert!(
                result
                    .as_array()
                    .is_some_and(|locations| !locations.is_empty()),
                "official Kotlin LSP returned no type definition for {name}: {result}"
            );
            json!({
                "case": name,
                "result": result
            })
        })
        .collect::<Vec<_>>();
    let negative_type_definition_positions = [
        (
            "generic explicit local declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "val explicitGen"),
        ),
        (
            "generic inferred local declaration",
            type_definition_use_uri.as_str(),
            position_after_marker(type_definition_use_source, "val copiedGen"),
        ),
    ];
    let expected_negative_type_definitions = negative_type_definition_positions
        .iter()
        .map(|(name, uri, (line, character))| {
            let result = reference.type_definition(uri, *line, *character);
            assert!(
                result.is_null(),
                "official Kotlin LSP resolved negative type-definition case {name}: {result}"
            );
            json!({
                "case": name,
                "result": result
            })
        })
        .collect::<Vec<_>>();
    let hover_positions = [
        ("class declaration", basic_tokens_uri.as_str(), 0, 12),
        ("class reference", basic_tokens_uri.as_str(), 1, 17),
        (
            "constructor property declaration",
            basic_tokens_uri.as_str(),
            0,
            21,
        ),
        ("parameter reference", basic_tokens_uri.as_str(), 1, 33),
        ("property reference", basic_tokens_uri.as_str(), 1, 38),
        (
            "top-level function declaration",
            definition_target_uri.as_str(),
            1,
            5,
        ),
        ("literal expression", definition_target_uri.as_str(), 1, 20),
        (
            "cross-file function reference",
            definition_use_uri.as_str(),
            1,
            18,
        ),
        (
            "same-name top-level value",
            top_kinds_use_uri.as_str(),
            1,
            26,
        ),
        ("local value reference", locals_uri.as_str(), 3, 12),
        ("local function reference", locals_uri.as_str(), 3, 21),
        ("selected overload reference", overloads_uri.as_str(), 2, 21),
        (
            "member overload reference",
            member_overloads_uri.as_str(),
            4,
            39,
        ),
        (
            "member property reference",
            member_kinds_uri.as_str(),
            4,
            44,
        ),
        ("inherited member reference", inherited_uri.as_str(), 5, 37),
        (
            "extension function reference",
            extension_uri.as_str(),
            2,
            33,
        ),
        (
            "extension property reference",
            extension_property_uri.as_str(),
            3,
            32,
        ),
        (
            "generic extension reference",
            generic_extension_uri.as_str(),
            1,
            28,
        ),
        (
            "object member reference",
            object_members_uri.as_str(),
            4,
            28,
        ),
        (
            "companion member reference",
            member_staticness_uri.as_str(),
            6,
            54,
        ),
        ("super member reference", super_call_uri.as_str(), 7, 29),
        (
            "imported class terminal",
            import_terminal_uri.as_str(),
            1,
            16,
        ),
        (
            "generic open class declaration",
            hover_advanced_uri.as_str(),
            1,
            12,
        ),
        (
            "open nullable property declaration",
            hover_advanced_uri.as_str(),
            2,
            12,
        ),
        (
            "defaulted interface method declaration",
            hover_advanced_uri.as_str(),
            6,
            7,
        ),
        (
            "bounded suspend function declaration",
            hover_advanced_uri.as_str(),
            8,
            22,
        ),
        (
            "inferred function declaration",
            hover_advanced_uri.as_str(),
            9,
            5,
        ),
        (
            "generic class reference",
            hover_advanced_uri.as_str(),
            10,
            14,
        ),
        (
            "protected vararg member declaration",
            hover_advanced_uri.as_str(),
            3,
            22,
        ),
        (
            "override member declaration",
            hover_inference_uri.as_str(),
            3,
            37,
        ),
        (
            "getter-only inferred property",
            hover_inference_uri.as_str(),
            4,
            5,
        ),
        (
            "inferred local function",
            hover_inference_uri.as_str(),
            7,
            7,
        ),
        (
            "inferred delegated local",
            hover_inference_uri.as_str(),
            8,
            7,
        ),
        ("enum entry declaration", member_tokens_uri.as_str(), 0, 20),
        ("bounded type parameter", hover_advanced_uri.as_str(), 8, 13),
        ("for-loop variable", hover_inference_uri.as_str(), 12, 8),
        ("lambda parameter", hover_inference_uri.as_str(), 16, 28),
        ("backticked function", backticked_uri.as_str(), 0, 5),
        (
            "backticked function parameter signature",
            backticked_uri.as_str(),
            2,
            5,
        ),
        (
            "backticked function parameter declaration",
            backticked_uri.as_str(),
            2,
            21,
        ),
        (
            "backticked function parameter reference",
            backticked_uri.as_str(),
            2,
            49,
        ),
        (
            "backticked constructor property",
            backticked_members_uri.as_str(),
            0,
            17,
        ),
        (
            "backticked enum entry",
            backticked_members_uri.as_str(),
            2,
            24,
        ),
        ("catch variable", hover_inference_uri.as_str(), 19, 42),
        (
            "block getter inferred property",
            hover_inference_uri.as_str(),
            20,
            5,
        ),
        (
            "top-level inferred delegated property",
            hover_inference_uri.as_str(),
            21,
            5,
        ),
        (
            "member inferred delegated property",
            hover_inference_uri.as_str(),
            22,
            29,
        ),
        ("fun interface", hover_modifiers_uri.as_str(), 1, 15),
        ("value class", hover_modifiers_uri.as_str(), 2, 24),
        ("inner class", hover_modifiers_uri.as_str(), 3, 28),
        (
            "enum constructor signature",
            hover_modifiers_uri.as_str(),
            4,
            12,
        ),
        (
            "annotation constructor signature",
            hover_modifiers_uri.as_str(),
            5,
            18,
        ),
        ("const property", hover_modifiers_uri.as_str(), 6, 11),
        ("lateinit property", hover_modifiers_uri.as_str(), 7, 14),
        ("override property", hover_modifiers_uri.as_str(), 9, 40),
        ("inline function", hover_modifiers_uri.as_str(), 10, 12),
        ("tailrec function", hover_modifiers_uri.as_str(), 11, 13),
        ("operator function", hover_modifiers_uri.as_str(), 12, 20),
        (
            "qualified inner class reference",
            hover_modifiers_uri.as_str(),
            13,
            42,
        ),
        (
            "inner class member reference",
            hover_modifiers_uri.as_str(),
            13,
            50,
        ),
        ("imported inner class type", nested_use_uri.as_str(), 3, 20),
        (
            "imported inner member reference",
            nested_use_uri.as_str(),
            3,
            40,
        ),
        (
            "imported outer inner constructor",
            nested_use_uri.as_str(),
            4,
            44,
        ),
        (
            "fully qualified inner class type",
            nested_use_uri.as_str(),
            5,
            45,
        ),
        (
            "fully qualified inner member reference",
            nested_use_uri.as_str(),
            5,
            65,
        ),
        (
            "member function wins over same-named inner class",
            nested_callable_collision_uri.as_str(),
            2,
            36,
        ),
        (
            "hover nested type through imported outer class",
            nested_outer_import_use_uri.as_str(),
            2,
            32,
        ),
        (
            "hover nested type through wildcard-imported outer class",
            nested_outer_wildcard_use_uri.as_str(),
            2,
            32,
        ),
        (
            "hover simple nested type inside outer class",
            nested_enclosing_use_uri.as_str(),
            3,
            20,
        ),
        (
            "inferred nested local type",
            hover_review_uri.as_str(),
            5,
            10,
        ),
        (
            "destructured component type",
            hover_review_uri.as_str(),
            9,
            10,
        ),
        (
            "multiline operator modifier",
            hover_review_uri.as_str(),
            13,
            11,
        ),
        (
            "multiline override modifier",
            hover_review_uri.as_str(),
            17,
            7,
        ),
    ];
    let expected_hovers = hover_positions
        .iter()
        .map(|(name, uri, line, character)| {
            json!({
                "case": name,
                "result": reference.hover(uri, *line, *character)
            })
        })
        .collect::<Vec<_>>();
    let reference_positions = [
        (
            "cross-file function including declaration",
            definition_use_uri.as_str(),
            1,
            18,
            true,
        ),
        (
            "cross-file function excluding declaration",
            definition_use_uri.as_str(),
            1,
            18,
            false,
        ),
        (
            "local value including declaration",
            locals_uri.as_str(),
            3,
            12,
            true,
        ),
        (
            "class type references including declaration",
            basic_tokens_uri.as_str(),
            1,
            17,
            true,
        ),
        (
            "selected overload only",
            overloads_uri.as_str(),
            2,
            21,
            true,
        ),
        (
            "same-name local value only",
            local_kinds_uri.as_str(),
            3,
            9,
            true,
        ),
        (
            "imported class through import terminal",
            import_terminal_uri.as_str(),
            1,
            16,
            true,
        ),
    ];
    let expected_references = reference_positions
        .iter()
        .map(|(name, uri, line, character, include_declaration)| {
            json!({
                "case": name,
                "result": reference.references(
                    uri,
                    *line,
                    *character,
                    *include_declaration
                )
            })
        })
        .collect::<Vec<_>>();
    let rename_unicode_position = position_after_marker(rename_unicode_source, "return ");
    let backticked_rename_position =
        position_after_marker(backticked_source, "fun useOdd(): Int = `");
    let rename_positions = [
        (
            "cross-file function",
            definition_use_uri.as_str(),
            1,
            18,
            "renamedAnswer",
        ),
        ("local value", locals_uri.as_str(), 3, 12, "renamedLocal"),
        (
            "selected overload",
            overloads_uri.as_str(),
            2,
            21,
            "renamedSelect",
        ),
        (
            "UTF-16 location after supplementary character",
            rename_unicode_uri.as_str(),
            rename_unicode_position.0,
            rename_unicode_position.1,
            "renamedTarget",
        ),
        (
            "backticked function to plain identifier",
            backticked_uri.as_str(),
            backticked_rename_position.0,
            backticked_rename_position.1,
            "plainName",
        ),
    ];
    let expected_renames = rename_positions
        .iter()
        .map(|(name, uri, line, character, new_name)| {
            json!({
                "case": name,
                "result": reference.rename(uri, *line, *character, new_name)
            })
        })
        .collect::<Vec<_>>();
    assert!(
        expected_renames
            .iter()
            .all(|case| case["result"].is_object()),
        "official Kotlin LSP returned no rename edits: {expected_renames:?}"
    );
    let completion_positions = [
        ("receiver members", completion_parity_uri.as_str(), 11, 18),
        (
            "unqualified lexical and top-level symbols",
            completion_parity_uri.as_str(),
            12,
            21,
        ),
    ];
    let expected_completions = completion_positions
        .iter()
        .map(|(name, uri, line, character)| {
            json!({
                "case": name,
                "result": reference.completions(
                    uri,
                    *line,
                    *character,
                    "krustyParity"
                )
            })
        })
        .collect::<Vec<_>>();
    let signature_help_positions = [
        (
            "top-level first argument",
            position_after_marker(signature_help_source, "combine(\"top"),
        ),
        (
            "top-level second argument",
            position_after_marker(signature_help_source, "combine(\"top\", "),
        ),
        (
            "member first argument",
            position_after_marker(signature_help_source, "box.member(\"member"),
        ),
        (
            "member second argument",
            position_after_marker(signature_help_source, "box.member(\"member\", "),
        ),
        (
            "constructor first argument",
            position_after_marker(signature_help_source, "Box(\"box"),
        ),
        (
            "constructor second argument",
            position_after_marker(signature_help_source, "Box(\"box\", "),
        ),
        (
            "selected integer overload",
            position_after_marker(signature_help_source, "combine(1"),
        ),
        (
            "named count argument",
            position_after_marker(signature_help_source, "combine(count"),
        ),
        (
            "named left argument",
            position_after_marker(signature_help_source, "count = 2, left"),
        ),
        (
            "nested outer call",
            position_after_marker(signature_help_source, "val nested = combine("),
        ),
        (
            "nested inner call",
            position_after_marker(signature_help_source, "combine(combine("),
        ),
        (
            "generic source call",
            position_after_marker(signature_help_source, "val generic = identity("),
        ),
        (
            "unicode parameter labels",
            position_after_marker(signature_help_source, "val nonAscii = unicode("),
        ),
        (
            "vararg third argument",
            position_after_marker(signature_help_source, "val variadic = collect(1, 2, "),
        ),
        (
            "zero argument call",
            position_after_marker(signature_help_source, "val empty = zero("),
        ),
        (
            "local function call",
            position_after_marker(signature_help_source, "localResult = local(\"local"),
        ),
        (
            "selected secondary constructor overload",
            position_after_marker(signature_help_source, "Choice(1, "),
        ),
        (
            "class without a primary constructor",
            position_after_marker(signature_help_source, "SecondaryOnly("),
        ),
        (
            "constructor subtype overload",
            position_after_marker(signature_help_source, "subtypeSelected = SubtypeChoice("),
        ),
        (
            "nested generic substitution",
            position_after_marker(signature_help_source, "nestedGeneric = unwrap("),
        ),
        (
            "bare nullable generic substitution",
            position_after_marker(signature_help_source, "nullableIdentity = identity("),
        ),
        (
            "nested nullable generic substitution",
            position_after_marker(signature_help_source, "nullableHolder = unwrap("),
        ),
        (
            "merged nullable generic constraints",
            position_after_marker(signature_help_source, "mergedNullable = choose("),
        ),
        (
            "mixed generic constraints",
            position_after_marker(signature_help_source, "mixed = choose("),
        ),
        (
            "reversed mixed generic constraints",
            position_after_marker(signature_help_source, "mixedReversed = choose("),
        ),
        (
            "nullable generic substitution",
            position_after_marker(signature_help_source, "nullableGeneric = coalesce("),
        ),
    ];
    let mut expected_signature_help = signature_help_positions
        .iter()
        .map(|(name, (line, character))| {
            json!({
                "case": name,
                "result": reference.signature_help(
                    &signature_help_uri,
                    *line,
                    *character
                )
            })
        })
        .collect::<Vec<_>>();
    let (incomplete_line, incomplete_character) =
        position_after_marker(incomplete_signature_help_source, "combine(\"x\", ");
    expected_signature_help.push(json!({
        "case": "incomplete argument list",
        "result": reference.signature_help(
            &incomplete_signature_help_uri,
            incomplete_line,
            incomplete_character
        )
    }));
    assert!(
        expected_signature_help
            .iter()
            .all(|case| case["result"].is_object()),
        "official Kotlin LSP returned no signature help: {expected_signature_help:?}"
    );
    let incremental_changes = json!([
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
    ]);
    reference.change_document(&incremental_parity_uri, 2, incremental_changes.clone());
    let expected_incremental_definition = reference.definition(&incremental_parity_uri, 1, 18);
    assert!(
        expected_incremental_definition
            .as_array()
            .is_some_and(|locations| !locations.is_empty()),
        "official Kotlin LSP returned no definition after incremental edits"
    );
    let expected_document_symbols = [
        &document_symbols_uri,
        &advanced_document_symbols_uri,
        &scoped_document_symbols_uri,
        &kind_document_symbols_uri,
    ]
    .map(|uri| reference.document_symbols(uri));
    assert!(
        expected_document_symbols.iter().all(|symbols| symbols
            .as_array()
            .is_some_and(|symbols| !symbols.is_empty())),
        "official Kotlin LSP returned no document symbols"
    );
    let expected_folding_ranges = reference.folding_ranges(&folding_ranges_uri);
    let expected_advanced_folding_ranges = reference.folding_ranges(&advanced_folding_ranges_uri);
    assert!(
        expected_folding_ranges
            .as_array()
            .is_some_and(|ranges| !ranges.is_empty())
            && expected_advanced_folding_ranges
                .as_array()
                .is_some_and(|ranges| !ranges.is_empty()),
        "official Kotlin LSP returned no folding ranges"
    );
    // The official server uses a multi-gigabyte IntelliJ process. Tear it down before starting
    // krusty so the opt-in differential does not retain both servers at peak memory.
    drop(reference);

    let mut krusty = LspProcess::spawn(env!("CARGO_BIN_EXE_krusty-lsp"), &["--stdio", "-no-jdk"]);
    let krusty_legend = krusty.initialize(&root_uri);
    let actual_diagnostics = diagnostic_cases
        .iter()
        .map(|(name, source)| {
            let uri = format!("file://{}", source_root.join(name).display());
            normalized_diagnostics(krusty.diagnostics_allow_empty(&uri, source))
        })
        .collect::<Vec<_>>();
    let actual_clean_diagnostics = clean_diagnostic_cases
        .iter()
        .map(|(name, source)| {
            let uri = format!("file://{}", source_root.join(name).display());
            normalized_diagnostics(krusty.diagnostics_allow_empty(&uri, source))
        })
        .collect::<Vec<_>>();
    let actual_tokens = token_cases
        .iter()
        .map(|(name, source)| {
            let uri = format!("file://{}", source_root.join(name).display());
            krusty.semantic_tokens(&uri, source, &krusty_legend)
        })
        .collect::<Vec<_>>();
    for (name, source) in definition_files {
        let uri = format!("file://{}", source_root.join(name).display());
        krusty.open_document(&uri, source);
    }
    let actual_definitions = definition_positions
        .iter()
        .map(|(name, uri, line, character)| {
            json!({
                "case": name,
                "result": krusty.definition(uri, *line, *character)
            })
        })
        .collect::<Vec<_>>();
    let actual_negative_definitions = negative_definition_positions
        .iter()
        .map(|(name, uri, line, character)| {
            json!({
                "case": name,
                "result": krusty.definition(uri, *line, *character)
            })
        })
        .collect::<Vec<_>>();
    let actual_type_definitions = type_definition_positions
        .iter()
        .map(|(name, uri, (line, character))| {
            json!({
                "case": name,
                "result": krusty.type_definition(
                    uri,
                    *line,
                    *character
                )
            })
        })
        .collect::<Vec<_>>();
    let actual_negative_type_definitions = negative_type_definition_positions
        .iter()
        .map(|(name, uri, (line, character))| {
            json!({
                "case": name,
                "result": krusty.type_definition(
                    uri,
                    *line,
                    *character
                )
            })
        })
        .collect::<Vec<_>>();
    let actual_hovers = hover_positions
        .iter()
        .map(|(name, uri, line, character)| {
            json!({
                "case": name,
                "result": krusty.hover(uri, *line, *character)
            })
        })
        .collect::<Vec<_>>();
    let actual_references = reference_positions
        .iter()
        .map(|(name, uri, line, character, include_declaration)| {
            json!({
                "case": name,
                "result": krusty.references(
                    uri,
                    *line,
                    *character,
                    *include_declaration
                )
            })
        })
        .collect::<Vec<_>>();
    let actual_renames = rename_positions
        .iter()
        .map(|(name, uri, line, character, new_name)| {
            json!({
                "case": name,
                "result": krusty.rename(uri, *line, *character, new_name)
            })
        })
        .collect::<Vec<_>>();
    let actual_completions = completion_positions
        .iter()
        .map(|(name, uri, line, character)| {
            json!({
                "case": name,
                "result": krusty.completions(
                    uri,
                    *line,
                    *character,
                    "krustyParity"
                )
            })
        })
        .collect::<Vec<_>>();
    let mut actual_signature_help = signature_help_positions
        .iter()
        .map(|(name, (line, character))| {
            json!({
                "case": name,
                "result": krusty.signature_help(
                    &signature_help_uri,
                    *line,
                    *character
                )
            })
        })
        .collect::<Vec<_>>();
    actual_signature_help.push(json!({
        "case": "incomplete argument list",
        "result": krusty.signature_help(
            &incomplete_signature_help_uri,
            incomplete_line,
            incomplete_character
        )
    }));
    krusty.change_document(&incremental_parity_uri, 2, incremental_changes);
    let actual_incremental_definition = krusty.definition(&incremental_parity_uri, 1, 18);
    let actual_document_symbols = [
        &document_symbols_uri,
        &advanced_document_symbols_uri,
        &scoped_document_symbols_uri,
        &kind_document_symbols_uri,
    ]
    .map(|uri| krusty.document_symbols(uri));
    let actual_folding_ranges = krusty.folding_ranges(&folding_ranges_uri);
    let actual_advanced_folding_ranges = krusty.folding_ranges(&advanced_folding_ranges_uri);

    for ((name, _), (actual, expected)) in diagnostic_cases
        .iter()
        .zip(actual_diagnostics.iter().zip(&expected_diagnostics))
    {
        assert_eq!(actual, expected, "diagnostic mismatch for {name}");
    }
    for ((name, _), (actual, expected)) in clean_diagnostic_cases.iter().zip(
        actual_clean_diagnostics
            .iter()
            .zip(&expected_clean_diagnostics),
    ) {
        assert_eq!(actual, expected, "clean diagnostic mismatch for {name}");
    }
    for ((name, _), (actual, expected)) in token_cases
        .iter()
        .zip(actual_tokens.iter().zip(&expected_tokens))
    {
        assert_eq!(actual, expected, "semantic-token mismatch for {name}");
    }
    assert_eq!(
        actual_definitions, expected_definitions,
        "definition mismatches"
    );
    assert_eq!(
        actual_negative_definitions, expected_negative_definitions,
        "negative definition mismatches"
    );
    assert_eq!(
        actual_type_definitions, expected_type_definitions,
        "type-definition mismatches"
    );
    assert_eq!(
        actual_negative_type_definitions, expected_negative_type_definitions,
        "negative type-definition mismatches"
    );
    assert_eq!(actual_hovers, expected_hovers, "hover mismatches");
    assert_eq!(
        actual_references, expected_references,
        "reference mismatches"
    );
    assert_eq!(actual_renames, expected_renames, "rename mismatches");
    assert_eq!(
        actual_completions.len(),
        expected_completions.len(),
        "completion case count mismatch"
    );
    for (actual, expected) in actual_completions.iter().zip(expected_completions.iter()) {
        assert_eq!(
            actual["case"], expected["case"],
            "completion case order mismatch"
        );
        let case = &actual["case"];
        assert_eq!(
            expected["result"]["isIncomplete"],
            json!(true),
            "official Kotlin LSP is expected to refine completion server-side for {case}"
        );
        assert_eq!(
            actual["result"]["isIncomplete"],
            json!(false),
            "krusty returns a client-filterable completion list for {case}"
        );
        assert_eq!(
            actual["result"]["items"], expected["result"]["items"],
            "completion item mismatch for {case}"
        );
    }
    assert_eq!(
        actual_signature_help, expected_signature_help,
        "signature-help mismatches"
    );
    assert_eq!(
        actual_incremental_definition, expected_incremental_definition,
        "incremental synchronization definition/range mismatch"
    );
    for ((name, actual), expected) in ["basic", "advanced", "scopes", "kinds"]
        .into_iter()
        .zip(actual_document_symbols)
        .zip(expected_document_symbols)
    {
        assert_eq!(actual, expected, "document symbol mismatch for {name}");
    }
    assert_eq!(
        actual_folding_ranges, expected_folding_ranges,
        "basic folding-range mismatch"
    );
    assert_eq!(
        actual_advanced_folding_ranges, expected_advanced_folding_ranges,
        "advanced folding-range mismatch"
    );
}
