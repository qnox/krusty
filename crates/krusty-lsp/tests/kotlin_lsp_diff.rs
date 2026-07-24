//! Opt-in protocol differential against JetBrains' official Kotlin LSP.
//!
//! Set `KRUSTY_KOTLIN_LSP` to the official launcher (`kotlin-lsp.sh` or
//! `bin/intellij-server`). The official distribution is intentionally not downloaded by the normal
//! test suite: it is large, platform-specific, and released independently from krusty.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use krusty_lsp::{read_framed, write_framed, MAX_MESSAGE_BYTES};
use serde_json::{json, Value};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const ANALYSIS_TIMEOUT: Duration = Duration::from_secs(300);

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

struct TempProject {
    root: PathBuf,
}

impl TempProject {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("krusty_lsp_diff_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
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

    fn next_request_id(&mut self) -> i64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
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

        // The first opt-in run may need to download Gradle and import/index a cold project.
        let deadline = Instant::now() + ANALYSIS_TIMEOUT;
        loop {
            if let Some(index) = self.pending.iter().position(|message| {
                message["method"] == "textDocument/publishDiagnostics"
                    && message["params"]["uri"] == uri
                    && message["params"]["diagnostics"]
                        .as_array()
                        .is_some_and(|items| !items.is_empty())
            }) {
                return self.pending.swap_remove(index)["params"]["diagnostics"]
                    .as_array()
                    .unwrap()
                    .clone();
            }

            let request_id = self.next_request_id();
            let response = self.request(
                request_id,
                "textDocument/diagnostic",
                json!({"textDocument": {"uri": uri}}),
            );
            if let Some(items) = response["result"]["items"].as_array() {
                if !items.is_empty() {
                    return items.clone();
                }
            }
            assert!(
                Instant::now() < deadline,
                "LSP produced no diagnostics for {uri}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn semantic_tokens(&mut self, uri: &str, text: &str, legend: &SemanticLegend) -> Vec<Value> {
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
            json!({
                "range": diagnostic["range"],
                "severity": diagnostic["severity"],
                "source": diagnostic["source"],
                "message": diagnostic["message"],
            })
        })
        .collect::<Vec<_>>();
    diagnostics.sort_by_key(Value::to_string);
    diagnostics
}

#[test]
fn reference_version_selection_is_semantic_and_order_independent() {
    let manifest = "2.10.0 newer\n# ignored\n2.9.9 older\n";
    assert_eq!(reference_kotlin_version_from(manifest), "2.10.0");
}

#[test]
fn diagnostics_and_semantic_tokens_match_official_kotlin_lsp() {
    let Ok(kotlin_lsp) = std::env::var("KRUSTY_KOTLIN_LSP") else {
        eprintln!("skipping Kotlin LSP differential: set KRUSTY_KOTLIN_LSP");
        return;
    };

    let project = TempProject::new();
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
            "ConditionType.kt",
            "fun conditionMismatch(): Int { if (1) return 1; return 0 }\n",
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
    ];
    for (name, source) in diagnostic_cases.iter().chain(&token_cases) {
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
    let expected_tokens = token_cases
        .iter()
        .map(|(name, source)| {
            let uri = format!("file://{}", source_root.join(name).display());
            reference.semantic_tokens(&uri, source, &reference_legend)
        })
        .collect::<Vec<_>>();
    // The official server uses a multi-gigabyte IntelliJ process. Tear it down before starting
    // krusty so the opt-in differential does not retain both servers at peak memory.
    drop(reference);

    let mut krusty = LspProcess::spawn(env!("CARGO_BIN_EXE_krusty-lsp"), &["--stdio", "-no-jdk"]);
    let krusty_legend = krusty.initialize(&root_uri);
    let actual_diagnostics = diagnostic_cases
        .iter()
        .map(|(name, source)| {
            let uri = format!("file://{}", source_root.join(name).display());
            normalized_diagnostics(krusty.diagnostics(&uri, source))
        })
        .collect::<Vec<_>>();
    let actual_tokens = token_cases
        .iter()
        .map(|(name, source)| {
            let uri = format!("file://{}", source_root.join(name).display());
            krusty.semantic_tokens(&uri, source, &krusty_legend)
        })
        .collect::<Vec<_>>();

    for ((name, _), (actual, expected)) in diagnostic_cases
        .iter()
        .zip(actual_diagnostics.iter().zip(&expected_diagnostics))
    {
        assert_eq!(actual, expected, "diagnostic mismatch for {name}");
    }
    for ((name, _), (actual, expected)) in token_cases
        .iter()
        .zip(actual_tokens.iter().zip(&expected_tokens))
    {
        assert_eq!(actual, expected, "semantic-token mismatch for {name}");
    }
}
