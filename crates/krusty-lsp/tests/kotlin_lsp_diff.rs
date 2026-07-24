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
        self.open_document(uri, text);

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
fn diagnostics_semantic_tokens_and_definitions_match_official_kotlin_lsp() {
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
        (
            "Backticked.kt",
            "fun `odd name`(): Int = 1\n\
             fun useOdd(): Int = `odd name`()\n",
        ),
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
    ];
    for (name, source) in diagnostic_cases
        .iter()
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
    let expected_tokens = token_cases
        .iter()
        .map(|(name, source)| {
            let uri = format!("file://{}", source_root.join(name).display());
            reference.semantic_tokens(&uri, source, &reference_legend)
        })
        .collect::<Vec<_>>();
    let basic_tokens_uri = format!("file://{}", source_root.join("BasicTokens.kt").display());
    for (name, source) in definition_files {
        let uri = format!("file://{}", source_root.join(name).display());
        reference.open_document(&uri, source);
    }
    let definition_use_uri = format!("file://{}", source_root.join("DefinitionUse.kt").display());
    let locals_uri = format!("file://{}", source_root.join("Locals.kt").display());
    let local_kinds_uri = format!("file://{}", source_root.join("LocalKinds.kt").display());
    let top_kinds_use_uri = format!("file://{}", source_root.join("TopKindsUse.kt").display());
    let receiverless_extension_uri = format!(
        "file://{}",
        source_root.join("ReceiverlessExtension.kt").display()
    );
    let overloads_uri = format!("file://{}", source_root.join("Overloads.kt").display());
    let member_overloads_uri = format!(
        "file://{}",
        source_root.join("MemberOverloads.kt").display()
    );
    let inherited_uri = format!("file://{}", source_root.join("Inherited.kt").display());
    let body_property_uri = format!("file://{}", source_root.join("BodyProperty.kt").display());
    let backticked_uri = format!("file://{}", source_root.join("Backticked.kt").display());
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
    assert_eq!(
        actual_definitions, expected_definitions,
        "definition mismatches"
    );
    assert_eq!(
        actual_negative_definitions, expected_negative_definitions,
        "negative definition mismatches"
    );
}
