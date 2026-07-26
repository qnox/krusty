//! Build Server Protocol project-model provider.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use super::fingerprint::collect_build_files;
use super::model::{
    Module, ModuleId, ModuleOutput, ProjectModel, ProviderKind, SourceRoot, SourceRootKind,
};
use super::provider::{ProbeError, ProjectProvider};
use super::runner::CommandRunner;
use crate::server::{read_framed, write_framed};
use crate::uri::{file_uri_or_path, path_to_file_uri};

const MAX_BSP_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
const BSP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CLIENT_NAME: &str = "krusty-lsp";
const BSP_VERSION: &str = "2.1.0";

fn is_bsp_relevant_file(path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let in_bsp_dir = path
        .parent()
        .and_then(|parent| parent.file_name())
        .is_some_and(|parent| parent == ".bsp");
    (in_bsp_dir && name.ends_with(".json"))
        || name.ends_with(".gradle")
        || name.ends_with(".gradle.kts")
        || name == "libs.versions.toml"
        || name == "pom.xml"
        || name == "BUILD"
        || name == "BUILD.bazel"
        || name.ends_with(".bzl")
        || name == "build.sbt"
        || name == "build.sc"
}

/// A `.bsp/*.json` connection file: the command that launches the build server.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BspConnection {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub languages: Vec<String>,
    pub argv: Vec<String>,
}

/// Find a workspace BSP connection, preferring one that declares Kotlin or Java.
pub fn discover(root: &Path) -> Option<BspConnection> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join(".bsp"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();

    let mut unspecified = None;
    for path in files {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(connection) = serde_json::from_str::<BspConnection>(&contents) else {
            continue;
        };
        if connection.argv.is_empty() {
            continue;
        }
        let handles_jvm = connection
            .languages
            .iter()
            .any(|language| language == "kotlin" || language == "java");
        if handles_jvm {
            return Some(connection);
        }
        if connection.languages.is_empty() {
            unspecified.get_or_insert(connection);
        }
    }
    unspecified
}

/// A JSON-RPC channel to a build server. The trait lets the handshake be tested against canned
/// responses, with the child-process transport used in production.
pub trait BspTransport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, ProbeError>;
    fn notify(&mut self, method: &str, params: Value) -> Result<(), ProbeError>;
    /// Best-effort teardown; errors are ignored because the model is already in hand.
    fn close(&mut self) {}
}

pub struct BspProvider {
    root: PathBuf,
    connection: BspConnection,
}

impl BspProvider {
    pub fn new(root: impl Into<PathBuf>, connection: BspConnection) -> Self {
        Self {
            root: root.into(),
            connection,
        }
    }

    /// Drive the BSP handshake and queries over `transport`, mapping the results to a model.
    fn probe_with(&self, transport: &mut dyn BspTransport) -> Result<ProjectModel, ProbeError> {
        let root_uri = path_to_file_uri(&self.root).ok_or_else(|| {
            ProbeError::Parse(format!(
                "workspace root is not an absolute file path: {}",
                self.root.display()
            ))
        })?;
        let initialize = transport.request(
            "build/initialize",
            json!({
                "displayName": CLIENT_NAME,
                "version": env!("CARGO_PKG_VERSION"),
                "bspVersion": BSP_VERSION,
                "rootUri": root_uri,
                "capabilities": { "languageIds": ["kotlin", "java"] },
            }),
        )?;
        let output_paths_supported = initialize
            .pointer("/capabilities/outputPathsProvider")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        transport.notify("build/initialized", json!({}))?;

        let targets_response = transport.request("workspace/buildTargets", json!({}))?;
        let targets: BuildTargets = parse(targets_response)?;
        let target_ids: Vec<Value> = targets
            .targets
            .iter()
            .filter(|target| target.is_jvm())
            .map(|target| json!({ "uri": target.id.uri }))
            .collect();

        let sources: SourcesResult =
            parse(transport.request("buildTarget/sources", json!({ "targets": target_ids }))?)?;
        let classpath: ClasspathResult = parse(transport.request(
            "buildTarget/jvmCompileClasspath",
            json!({ "targets": target_ids }),
        )?)?;
        let output_paths = if output_paths_supported {
            parse(transport.request("buildTarget/outputPaths", json!({ "targets": target_ids }))?)?
        } else {
            OutputPathsResult::default()
        };

        transport.request("build/shutdown", json!({})).ok();
        transport.notify("build/exit", json!({})).ok();
        transport.close();

        Ok(model_from_responses(
            &self.root,
            &targets,
            &sources,
            &classpath,
            &output_paths,
        ))
    }
}

impl ProjectProvider for BspProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Bsp
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        let mut watched = collect_build_files(&self.root, 8, &is_bsp_relevant_file);
        watched.sort();
        watched
    }

    fn probe(&self, _runner: &dyn CommandRunner) -> Result<ProjectModel, ProbeError> {
        let mut transport = ChildTransport::spawn(&self.connection.argv, &self.root)?;
        let model = self.probe_with(&mut transport);
        transport.close();
        model
    }
}

#[derive(Debug, Deserialize)]
struct BuildTargets {
    #[serde(default)]
    targets: Vec<BuildTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildTarget {
    id: TargetId,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    base_directory: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    dependencies: Vec<TargetId>,
    #[serde(default)]
    language_ids: Vec<String>,
    #[serde(default)]
    data_kind: Option<String>,
    #[serde(default)]
    data: Option<JvmData>,
}

impl BuildTarget {
    fn is_jvm(&self) -> bool {
        self.data_kind.as_deref() == Some("jvm")
            || self
                .language_ids
                .iter()
                .any(|language| language == "kotlin" || language == "java")
    }
}

#[derive(Debug, Deserialize)]
struct TargetId {
    uri: String,
}

/// The `data` of a target whose `dataKind` is `jvm`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JvmData {
    #[serde(default)]
    java_home: Option<String>,
    #[serde(default)]
    java_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SourcesResult {
    #[serde(default)]
    items: Vec<SourcesItem>,
}

#[derive(Debug, Deserialize)]
struct SourcesItem {
    target: TargetId,
    #[serde(default)]
    sources: Vec<SourceItem>,
}

#[derive(Debug, Deserialize)]
struct SourceItem {
    uri: String,
    /// 1 = file, 2 = directory.
    #[serde(default)]
    kind: u8,
    #[serde(default)]
    generated: bool,
}

#[derive(Debug, Deserialize)]
struct ClasspathResult {
    #[serde(default)]
    items: Vec<ClasspathItem>,
}

#[derive(Debug, Deserialize)]
struct ClasspathItem {
    target: TargetId,
    #[serde(default)]
    classpath: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutputPathsResult {
    #[serde(default)]
    items: Vec<OutputPathsItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutputPathsItem {
    target: TargetId,
    #[serde(default)]
    output_paths: Vec<OutputPathItem>,
}

#[derive(Debug, Deserialize)]
struct OutputPathItem {
    uri: String,
}

fn model_from_responses(
    root: &Path,
    targets: &BuildTargets,
    sources: &SourcesResult,
    classpath: &ClasspathResult,
    output_paths: &OutputPathsResult,
) -> ProjectModel {
    let jdk_home = targets
        .targets
        .iter()
        .find_map(|target| {
            if !target.is_jvm() {
                return None;
            }
            target
                .data
                .as_ref()
                .and_then(|data| data.java_home.as_deref())
        })
        .and_then(file_uri_or_path);

    let mut modules = Vec::new();
    for target in targets.targets.iter().filter(|target| target.is_jvm()) {
        let is_test = target
            .tags
            .iter()
            .any(|tag| tag == "test" || tag == "integration-test");
        let kind = if is_test {
            SourceRootKind::Test
        } else {
            SourceRootKind::Source
        };

        let mut module = Module::new(
            ModuleId::raw(target.id.uri.clone()),
            target
                .base_directory
                .as_deref()
                .and_then(file_uri_or_path)
                .unwrap_or_else(|| root.to_path_buf()),
        );
        module.display_name = target
            .display_name
            .clone()
            .unwrap_or_else(|| target.id.uri.clone());
        module.source_roots = source_roots_for(&target.id.uri, sources, kind);
        module.classpath = classpath_for(&target.id.uri, classpath);
        module.outputs = output_paths_for(&target.id.uri, output_paths)
            .into_iter()
            .map(ModuleOutput::location)
            .collect();
        module.jvm_target = target
            .data
            .as_ref()
            .and_then(|data| data.java_version.clone());
        module.depends_on = target
            .dependencies
            .iter()
            .map(|dependency| ModuleId::raw(dependency.uri.clone()))
            .collect();
        modules.push(module);
    }

    ProjectModel {
        root: root.to_path_buf(),
        kind: ProviderKind::Bsp,
        jdk_home,
        modules,
    }
}

/// Source roots for a target: directories reported directly, and the parent directory of any source
/// reported as an individual file.
fn source_roots_for(
    target_uri: &str,
    sources: &SourcesResult,
    kind: SourceRootKind,
) -> Vec<SourceRoot> {
    let mut roots: Vec<SourceRoot> = Vec::new();
    let item = sources
        .items
        .iter()
        .find(|item| item.target.uri == target_uri);
    let Some(item) = item else {
        return roots;
    };
    for source in &item.sources {
        let Some(path) = file_uri_or_path(&source.uri) else {
            continue;
        };
        let directory = if source.kind == 1 {
            match path.parent() {
                Some(parent) => parent.to_path_buf(),
                None => continue,
            }
        } else {
            path
        };
        if roots.iter().any(|root| root.path == directory) {
            continue;
        }
        let root = SourceRoot {
            path: directory,
            kind,
            generated: source.generated,
        };
        roots.push(root);
    }
    roots
}

fn classpath_for(target_uri: &str, classpath: &ClasspathResult) -> Vec<PathBuf> {
    classpath
        .items
        .iter()
        .find(|item| item.target.uri == target_uri)
        .map(|item| {
            item.classpath
                .iter()
                .filter_map(|entry| file_uri_or_path(entry))
                .collect()
        })
        .unwrap_or_default()
}

fn output_paths_for(target_uri: &str, output_paths: &OutputPathsResult) -> Vec<PathBuf> {
    output_paths
        .items
        .iter()
        .find(|item| item.target.uri == target_uri)
        .map(|item| {
            item.output_paths
                .iter()
                .filter_map(|entry| file_uri_or_path(&entry.uri))
                .collect()
        })
        .unwrap_or_default()
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ProbeError> {
    serde_json::from_value(value).map_err(|error| ProbeError::Parse(error.to_string()))
}

struct ChildTransport {
    child: Child,
    stdin: ChildStdin,
    frames: Receiver<std::io::Result<Option<Vec<u8>>>>,
    next_id: i64,
}

impl ChildTransport {
    fn spawn(argv: &[String], working_directory: &Path) -> Result<Self, ProbeError> {
        let (program, arguments) = argv
            .split_first()
            .ok_or_else(|| ProbeError::Io("empty BSP argv".to_string()))?;
        let mut child = Command::new(program)
            .args(arguments)
            .current_dir(working_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| ProbeError::Io(format!("{program}: {error}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProbeError::Io("BSP server stdin unavailable".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProbeError::Io("BSP server stdout unavailable".to_string()))?;
        let (send, frames) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let frame = read_framed(&mut stdout, MAX_BSP_MESSAGE_BYTES);
                let done = !matches!(frame, Ok(Some(_)));
                if send.send(frame).is_err() || done {
                    return;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            frames,
            next_id: 0,
        })
    }
}

impl BspTransport for ChildTransport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, ProbeError> {
        self.next_id += 1;
        let id = self.next_id;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let body =
            serde_json::to_vec(&message).map_err(|error| ProbeError::Io(error.to_string()))?;
        write_framed(&mut self.stdin, &body).map_err(|error| ProbeError::Io(error.to_string()))?;

        let deadline = Instant::now() + BSP_REQUEST_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let frame = match self.frames.recv_timeout(remaining) {
                Ok(Ok(Some(frame))) => frame,
                Ok(Ok(None)) => {
                    return Err(ProbeError::Io(format!("BSP server closed during {method}")));
                }
                Ok(Err(error)) => return Err(ProbeError::Io(error.to_string())),
                Err(RecvTimeoutError::Timeout) => {
                    self.close();
                    return Err(ProbeError::Io(format!("BSP request {method} timed out")));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ProbeError::Io(format!("BSP server closed during {method}")));
                }
            };
            let message: Value = serde_json::from_slice(&frame)
                .map_err(|error| ProbeError::Parse(error.to_string()))?;
            if message.get("method").is_some() {
                if let Some(server_id) = message.get("id") {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": server_id,
                        "error": {"code": -32601, "message": "method not supported"},
                    });
                    let body = serde_json::to_vec(&response)
                        .map_err(|error| ProbeError::Io(error.to_string()))?;
                    write_framed(&mut self.stdin, &body)
                        .map_err(|error| ProbeError::Io(error.to_string()))?;
                }
                continue;
            }
            if message.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(ProbeError::Tool {
                    program: method.to_string(),
                    status: error.get("code").and_then(Value::as_i64).unwrap_or(-1) as i32,
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                });
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), ProbeError> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let body =
            serde_json::to_vec(&message).map_err(|error| ProbeError::Io(error.to_string()))?;
        write_framed(&mut self.stdin, &body).map_err(|error| ProbeError::Io(error.to_string()))
    }

    fn close(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::project::testing::TempTree;

    #[test]
    fn discovery_prefers_a_connection_file_that_lists_a_jvm_language() {
        let tree = TempTree::new("bsp-discover");
        tree.write(
            ".bsp/aaa-other.json",
            r#"{"name":"other","argv":["other"],"languages":["scala"]}"#,
        );
        tree.write(
            ".bsp/zzz-gradle.json",
            r#"{"name":"gradle","argv":["java","-jar","bsp.jar"],"languages":["kotlin","java"]}"#,
        );
        let connection = discover(tree.root()).unwrap();
        assert_eq!(connection.name, "gradle");
        assert_eq!(connection.argv, vec!["java", "-jar", "bsp.jar"]);
    }

    #[test]
    fn discovery_returns_none_without_a_bsp_directory() {
        let tree = TempTree::new("bsp-none");
        assert_eq!(discover(tree.root()), None);
    }

    #[test]
    fn discovery_ignores_a_server_that_declares_only_non_jvm_languages() {
        let tree = TempTree::new("bsp-non-jvm");
        tree.write(
            ".bsp/scala.json",
            r#"{"name":"scala","argv":["scala","bsp"],"languages":["scala"]}"#,
        );
        assert_eq!(discover(tree.root()), None);
    }

    #[test]
    fn the_watch_set_covers_the_connection_file_and_the_underlying_build_files() {
        use crate::project::fingerprint::fingerprint_files;

        let tree = TempTree::new("bsp-watch");
        tree.write(".bsp/gradle.json", r#"{"name":"gradle","argv":["g"]}"#);
        tree.write("build.gradle.kts", "dependencies {}");
        tree.write("app/BUILD.bazel", "kt_jvm_library()");
        let provider = BspProvider::new(
            tree.root(),
            BspConnection {
                name: "gradle".to_string(),
                version: String::new(),
                languages: vec!["kotlin".to_string()],
                argv: vec!["g".to_string()],
            },
        );

        let watched = provider.watch_paths();
        assert!(watched.contains(&tree.path(".bsp/gradle.json")));
        assert!(watched.contains(&tree.path("build.gradle.kts")));
        assert!(watched.contains(&tree.path("app/BUILD.bazel")));

        // Editing a build file changes the fingerprint, so a BSP-backed project re-probes rather
        // than staying frozen for the whole session.
        let salt = provider.fingerprint_salt();
        let before = fingerprint_files(&watched, &salt);
        tree.write("build.gradle.kts", "dependencies { implementation(x) }");
        assert_ne!(before, fingerprint_files(&watched, &salt));
    }

    /// Replays canned responses keyed by method, and records the request order.
    struct FakeTransport {
        responses: HashMap<String, Value>,
        requested: Vec<String>,
        notified: Vec<String>,
    }

    impl FakeTransport {
        fn new(responses: HashMap<String, Value>) -> Self {
            Self {
                responses,
                requested: Vec::new(),
                notified: Vec::new(),
            }
        }
    }

    impl BspTransport for FakeTransport {
        fn request(&mut self, method: &str, _params: Value) -> Result<Value, ProbeError> {
            self.requested.push(method.to_string());
            Ok(self.responses.get(method).cloned().unwrap_or(Value::Null))
        }

        fn notify(&mut self, method: &str, _params: Value) -> Result<(), ProbeError> {
            self.notified.push(method.to_string());
            Ok(())
        }
    }

    fn responses() -> HashMap<String, Value> {
        let mut map = HashMap::new();
        map.insert(
            "build/initialize".to_string(),
            json!({
                "displayName": "test",
                "version": "1",
                "bspVersion": "2.1.0",
                "capabilities": { "outputPathsProvider": true }
            }),
        );
        map.insert(
            "workspace/buildTargets".to_string(),
            json!({ "targets": [
                {
                    "id": { "uri": "file:///p/app?id=app%3Amain" },
                    "displayName": "app:main",
                    "baseDirectory": "file:///p/app",
                    "tags": ["library"],
                    "languageIds": ["kotlin"],
                    "dependencies": [{ "uri": "file:///p/core?id=core%3Amain" }],
                    "dataKind": "jvm",
                    "data": { "javaHome": "file:///jdk21", "javaVersion": "21" }
                },
                {
                    "id": { "uri": "file:///p/app?id=app%3Atest" },
                    "displayName": "app:test",
                    "baseDirectory": "file:///p/app",
                    "tags": ["test"],
                    "languageIds": ["kotlin"],
                    "dependencies": [{ "uri": "file:///p/app?id=app%3Amain" }]
                },
                {
                    "id": { "uri": "file:///p/native?id=native%3Amain" },
                    "displayName": "native:main",
                    "baseDirectory": "file:///p/native",
                    "languageIds": ["scala"],
                    "dependencies": []
                }
            ]}),
        );
        map.insert(
            "buildTarget/sources".to_string(),
            json!({ "items": [
                { "target": { "uri": "file:///p/app?id=app%3Amain" },
                  "sources": [{ "uri": "file:///p/app/src/main/kotlin", "kind": 2, "generated": false }] },
                { "target": { "uri": "file:///p/app?id=app%3Atest" },
                  "sources": [{ "uri": "file:///p/app/src/test/kotlin/AppTest.kt", "kind": 1, "generated": false }] }
            ]}),
        );
        map.insert(
            "buildTarget/jvmCompileClasspath".to_string(),
            json!({ "items": [
                { "target": { "uri": "file:///p/app?id=app%3Amain" },
                  "classpath": ["file:///m2/kotlin-stdlib.jar", "file:///p/core/build/classes"] },
                { "target": { "uri": "file:///p/app?id=app%3Atest" },
                  "classpath": ["file:///m2/junit.jar", "file:///outside/app/classes/"] }
            ]}),
        );
        map.insert(
            "buildTarget/outputPaths".to_string(),
            json!({ "items": [
                { "target": { "uri": "file:///p/app?id=app%3Amain" },
                  "outputPaths": [
                      { "uri": "file:///outside/app/classes/", "kind": 2 }
                  ] },
                { "target": { "uri": "file:///p/app?id=app%3Atest" },
                  "outputPaths": [
                      { "uri": "file:///outside/app/test-classes/", "kind": 2 }
                  ] }
            ]}),
        );
        map
    }

    #[test]
    fn the_handshake_runs_in_order_and_shuts_the_server_down() {
        let provider = BspProvider::new(
            "/p",
            BspConnection {
                name: "test".to_string(),
                version: String::new(),
                languages: vec!["kotlin".to_string()],
                argv: vec!["server".to_string()],
            },
        );
        let mut transport = FakeTransport::new(responses());
        provider.probe_with(&mut transport).unwrap();

        assert_eq!(
            transport.requested,
            vec![
                "build/initialize",
                "workspace/buildTargets",
                "buildTarget/sources",
                "buildTarget/jvmCompileClasspath",
                "buildTarget/outputPaths",
                "build/shutdown",
            ]
        );
        assert_eq!(transport.notified, vec!["build/initialized", "build/exit"]);
    }

    #[test]
    fn output_paths_are_requested_only_when_the_server_supports_them() {
        let provider = BspProvider::new(
            "/p",
            BspConnection {
                name: "test".to_string(),
                version: String::new(),
                languages: vec!["kotlin".to_string()],
                argv: vec!["server".to_string()],
            },
        );
        let mut responses = responses();
        responses.insert(
            "build/initialize".to_string(),
            json!({ "capabilities": { "outputPathsProvider": false } }),
        );
        let mut transport = FakeTransport::new(responses);

        let model = provider.probe_with(&mut transport).unwrap();

        assert!(transport
            .requested
            .iter()
            .all(|method| method != "buildTarget/outputPaths"));
        assert!(model.modules.iter().all(|module| module.outputs.is_empty()));
    }

    #[test]
    fn build_targets_map_to_modules_with_sources_classpath_and_jdk() {
        let provider = BspProvider::new(
            "/p",
            BspConnection {
                name: "test".to_string(),
                version: String::new(),
                languages: vec!["kotlin".to_string()],
                argv: vec!["server".to_string()],
            },
        );
        let mut transport = FakeTransport::new(responses());
        let model = provider.probe_with(&mut transport).unwrap();

        assert_eq!(model.kind, ProviderKind::Bsp);
        assert_eq!(model.jdk_home, Some(PathBuf::from("/jdk21")));

        let main = model
            .module(&ModuleId::raw("file:///p/app?id=app%3Amain"))
            .unwrap();
        assert_eq!(main.display_name, "app:main");
        assert_eq!(
            main.source_roots[0].path,
            PathBuf::from("/p/app/src/main/kotlin")
        );
        assert_eq!(main.source_roots[0].kind, SourceRootKind::Source);
        assert_eq!(main.jvm_target.as_deref(), Some("21"));
        assert_eq!(
            main.classpath,
            vec![
                PathBuf::from("/m2/kotlin-stdlib.jar"),
                PathBuf::from("/p/core/build/classes"),
            ]
        );
        assert_eq!(
            main.outputs,
            vec![ModuleOutput::location("/outside/app/classes")]
        );

        let test = model
            .module(&ModuleId::raw("file:///p/app?id=app%3Atest"))
            .unwrap();
        assert_eq!(test.source_roots[0].kind, SourceRootKind::Test);
        // A source reported as a file contributes its parent directory as the root.
        assert_eq!(
            test.source_roots[0].path,
            PathBuf::from("/p/app/src/test/kotlin")
        );
        assert!(test
            .depends_on
            .contains(&ModuleId::raw("file:///p/app?id=app%3Amain")));
        assert_eq!(
            model.compile_classpath(test),
            vec![
                PathBuf::from("/m2/junit.jar"),
                PathBuf::from("/outside/app/classes"),
            ]
        );
        assert!(model
            .module(&ModuleId::raw("file:///p/native?id=native%3Amain"))
            .is_none());
    }
}
