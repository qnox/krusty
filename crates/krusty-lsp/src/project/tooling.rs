//! In-process build-tool APIs used to fetch project metadata.
//!
//! Gradle goes through the Tooling API (`GradleConnector.newConnector`, `useBuildDistribution`,
//! `ProjectConnection.newBuild`). Maven goes through the resolver
//! (`RepositorySystem.resolveArtifact` / `resolveDependencies`). One JVM stays up for each API and
//! keeps its connection. Neither path starts `gradlew` or `mvn`.

use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;

use super::runner::{CommandOutput, SCAN_PROBE_TIMEOUT};

const GRADLE_CLIENT: &str = include_str!("../../java/krusty/tooling/GradleToolingClient.java");
const MAVEN_CLIENT: &str = include_str!("../../java/krusty/tooling/MavenResolverClient.java");
const PROTOCOL: &str = include_str!("../../java/krusty/tooling/Protocol.java");

/// How the Tooling API chooses a Gradle distribution. The project's wrapper is the only source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distribution {
    /// `GradleConnector.useBuildDistribution()`.
    BuildDistribution,
}

/// One `ProjectConnection.newBuild()` action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GradleBuildAction {
    pub project_dir: PathBuf,
    pub distribution: Distribution,
    pub tasks: Vec<String>,
    pub arguments: Vec<String>,
    pub timeout: Duration,
    /// JVM the project daemon uses to compile the injected init script.
    /// `None` keeps the client's JVM.
    pub java_home: Option<PathBuf>,
}

/// One coordinate for `RepositorySystem`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MavenArtifactRequest {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub extension: String,
    pub classifier: String,
    pub transitive: bool,
    pub exclusions: Vec<MavenExclusion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MavenExclusion {
    pub group: String,
    pub artifact: String,
}

/// A remote repository from the project's `jarRepositories.xml`. IntelliJ's embedder
/// resolves against these, not only Maven Central.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MavenRepository {
    pub id: String,
    pub url: String,
}

/// Every artifact of one resolve. The client sends this as a single request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MavenBatch {
    pub artifacts: Vec<MavenArtifactRequest>,
    pub repositories: Vec<MavenRepository>,
}

#[derive(Serialize)]
struct GradleWire<'a> {
    kind: &'static str,
    #[serde(rename = "projectDir")]
    project_dir: &'a Path,
    tasks: &'a [String],
    arguments: &'a [String],
    #[serde(rename = "javaHome", skip_serializing_if = "Option::is_none")]
    java_home: Option<&'a Path>,
}

#[derive(Serialize)]
struct MavenWire<'a> {
    kind: &'static str,
    artifacts: &'a [MavenArtifactRequest],
    repositories: &'a [MavenRepository],
}

#[derive(serde::Deserialize)]
struct ToolResponse {
    status: i32,
    stdout: String,
    stderr: String,
}

pub(super) fn gradle_request(action: &GradleBuildAction) -> io::Result<String> {
    serde_json::to_string(&GradleWire {
        kind: "gradle",
        project_dir: &action.project_dir,
        tasks: &action.tasks,
        arguments: &action.arguments,
        java_home: action.java_home.as_deref(),
    })
    .map_err(io::Error::other)
}

pub fn run_gradle(action: &GradleBuildAction) -> io::Result<CommandOutput> {
    debug_assert_eq!(action.distribution, Distribution::BuildDistribution);
    if let Some(response) = test_gradle(action) {
        return response;
    }
    let line = gradle_request(action)?;
    let response = transact(ClientKind::Gradle, &line, action.timeout)?;
    Ok(CommandOutput {
        status: response.status,
        stdout: response.stdout,
        stderr: response.stderr,
    })
}

pub fn resolve_maven(batch: &MavenBatch) -> io::Result<()> {
    if let Some(response) = test_maven(batch) {
        return response;
    }
    let line = serde_json::to_string(&MavenWire {
        kind: "maven",
        artifacts: &batch.artifacts,
        repositories: &batch.repositories,
    })
    .map_err(io::Error::other)?;
    // A missing artifact is recorded by the caller from the files on disk. The resolver still
    // walks every coordinate in this one request.
    transact(ClientKind::Maven, &line, SCAN_PROBE_TIMEOUT).map(|_| ())
}

fn test_gradle(action: &GradleBuildAction) -> Option<io::Result<CommandOutput>> {
    #[cfg(test)]
    {
        hooks::take_gradle(action)
    }
    #[cfg(not(test))]
    {
        let _ = action;
        None
    }
}

fn test_maven(batch: &MavenBatch) -> Option<io::Result<()>> {
    #[cfg(test)]
    {
        hooks::take_maven(batch)
    }
    #[cfg(not(test))]
    {
        let _ = batch;
        None
    }
}

#[cfg(test)]
mod hooks {
    use std::cell::RefCell;

    use super::*;

    thread_local! {
        static GRADLE_RESPONSE: RefCell<Option<io::Result<CommandOutput>>> = const { RefCell::new(None) };
        static GRADLE_ACTIONS: RefCell<Vec<GradleBuildAction>> = const { RefCell::new(Vec::new()) };
        static MAVEN_RESPONSE: RefCell<Option<io::Result<()>>> = const { RefCell::new(None) };
        static MAVEN_BATCHES: RefCell<Vec<MavenBatch>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn take_gradle(action: &GradleBuildAction) -> Option<io::Result<CommandOutput>> {
        GRADLE_RESPONSE.with(|slot| {
            let response = slot.borrow_mut().take()?;
            GRADLE_ACTIONS.with(|actions| actions.borrow_mut().push(action.clone()));
            Some(response)
        })
    }

    pub(super) fn take_maven(batch: &MavenBatch) -> Option<io::Result<()>> {
        MAVEN_RESPONSE.with(|slot| {
            let response = slot.borrow_mut().take()?;
            MAVEN_BATCHES.with(|batches| batches.borrow_mut().push(batch.clone()));
            Some(response)
        })
    }

    pub(super) fn reset() {
        GRADLE_RESPONSE.with(|slot| *slot.borrow_mut() = None);
        GRADLE_ACTIONS.with(|actions| actions.borrow_mut().clear());
        MAVEN_RESPONSE.with(|slot| *slot.borrow_mut() = None);
        MAVEN_BATCHES.with(|batches| batches.borrow_mut().clear());
    }

    pub(super) fn set_gradle(response: io::Result<CommandOutput>) {
        GRADLE_ACTIONS.with(|actions| actions.borrow_mut().clear());
        GRADLE_RESPONSE.with(|slot| *slot.borrow_mut() = Some(response));
    }

    pub(super) fn set_maven(response: io::Result<()>) {
        MAVEN_BATCHES.with(|batches| batches.borrow_mut().clear());
        MAVEN_RESPONSE.with(|slot| *slot.borrow_mut() = Some(response));
    }

    pub(super) fn gradle_actions() -> Vec<GradleBuildAction> {
        GRADLE_ACTIONS.with(|actions| actions.borrow().clone())
    }

    pub(super) fn maven_batches() -> Vec<MavenBatch> {
        MAVEN_BATCHES.with(|batches| batches.borrow().clone())
    }
}

#[cfg(test)]
pub(crate) fn reset_test_hooks() {
    hooks::reset();
}

#[cfg(test)]
pub(crate) fn set_gradle_response(response: io::Result<CommandOutput>) {
    hooks::set_gradle(response);
}

#[cfg(test)]
pub(crate) fn set_maven_response(response: io::Result<()>) {
    hooks::set_maven(response);
}

#[cfg(test)]
pub(crate) fn gradle_actions() -> Vec<GradleBuildAction> {
    hooks::gradle_actions()
}

#[cfg(test)]
pub(crate) fn maven_batches() -> Vec<MavenBatch> {
    hooks::maven_batches()
}

#[derive(Clone, Copy)]
enum ClientKind {
    Gradle,
    Maven,
}

struct LiveClient {
    child: Child,
    stdin: ChildStdin,
    incoming: mpsc::Receiver<io::Result<String>>,
    log: PathBuf,
}

impl Drop for LiveClient {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

fn slot(kind: ClientKind) -> &'static Mutex<Option<LiveClient>> {
    match kind {
        ClientKind::Gradle => {
            static SLOT: OnceLock<Mutex<Option<LiveClient>>> = OnceLock::new();
            SLOT.get_or_init(|| Mutex::new(None))
        }
        ClientKind::Maven => {
            static SLOT: OnceLock<Mutex<Option<LiveClient>>> = OnceLock::new();
            SLOT.get_or_init(|| Mutex::new(None))
        }
    }
}

/// The one stdout line the tooling client is allowed to emit. Maven prints
/// `[WARNING] The POM for ... is invalid` on stdout while a resolve is still running;
/// that line does not deserialize, and treating it as the response stops the batch.
fn is_tool_response(line: &str) -> bool {
    serde_json::from_str::<ToolResponse>(line).is_ok()
}

fn transact(kind: ClientKind, line: &str, timeout: Duration) -> io::Result<ToolResponse> {
    let mutex = slot(kind);
    let mut slot = mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let alive = slot
        .as_mut()
        .is_some_and(|client| matches!(client.child.try_wait(), Ok(None)));
    if !alive {
        *slot = Some(spawn(kind)?);
    }
    let write_error = {
        let client = slot.as_mut().expect("client");
        writeln!(client.stdin, "{line}").and_then(|()| client.stdin.flush())
    };
    if let Err(error) = write_error {
        let log = slot
            .as_ref()
            .map(|client| log_tail(&client.log))
            .unwrap_or_default();
        *slot = None;
        return Err(io::Error::other(format!("{error}{log}")));
    }
    let received = slot
        .as_mut()
        .expect("client")
        .incoming
        .recv_timeout(timeout);
    let log = slot
        .as_ref()
        .map(|client| log_tail(&client.log))
        .unwrap_or_default();
    match received {
        Ok(Ok(line)) => serde_json::from_str(&line).map_err(|error| {
            *slot = None;
            io::Error::other(format!("{error}: {line}{log}"))
        }),
        Ok(Err(error)) => {
            *slot = None;
            Err(io::Error::other(format!("{error}{log}")))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            *slot = None;
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("tooling client timed out{log}"),
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            *slot = None;
            Err(io::Error::other(format!("tooling client closed{log}")))
        }
    }
}

fn spawn(kind: ClientKind) -> io::Result<LiveClient> {
    let (main_class, sources, compile_jars, runtime_jars) = match kind {
        ClientKind::Gradle => {
            // The distribution `lib` is the Tooling API implementation classpath: the same set
            // IntelliJ loads for an installed Gradle. The split jars in that directory are not
            // optional; `gradle-tooling-api` alone cannot open a connection.
            let lib = gradle_distribution_lib()?;
            (
                "krusty.tooling.GradleToolingClient",
                vec![
                    ("Protocol.java", PROTOCOL),
                    ("GradleToolingClient.java", GRADLE_CLIENT),
                ],
                vec![tooling_api_jar(&lib)?],
                jars_in(&lib)?,
            )
        }
        ClientKind::Maven => {
            let jars = maven_classpath_jars()?;
            (
                "krusty.tooling.MavenResolverClient",
                vec![
                    ("Protocol.java", PROTOCOL),
                    ("MavenResolverClient.java", MAVEN_CLIENT),
                ],
                jars.clone(),
                jars,
            )
        }
    };
    let classes = compile(&sources, &compile_jars)?;
    let mut classpath = vec![classes.clone()];
    classpath.extend(runtime_jars);
    let classpath =
        std::env::join_paths(&classpath).map_err(|error| io::Error::other(error.to_string()))?;
    let log = classes.join("client.log");
    let log_file = std::fs::File::create(&log)?;
    let mut command = client_command(&java_bin("java"), &classpath.to_string_lossy(), main_class);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log_file));
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("tooling client stdout unavailable"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("tooling client stdin unavailable"))?;
    let (sender, incoming) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = sender.send(Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "tooling client closed",
                    )));
                    break;
                }
                Ok(_) => {
                    if line.ends_with('\n') {
                        line.pop();
                        if line.ends_with('\r') {
                            line.pop();
                        }
                    }
                    // A resolver warning is not the response. Waiting for the JSON line lets the
                    // rest of the batch finish; taking the warning ends the request.
                    if !is_tool_response(&line) {
                        continue;
                    }
                    if sender.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                    break;
                }
            }
        }
    });
    Ok(LiveClient {
        child,
        stdin,
        incoming,
        log,
    })
}

fn client_command(java: &Path, classpath: &str, main_class: &str) -> std::process::Command {
    let mut command = std::process::Command::new(java);
    command.arg("-cp").arg(classpath).arg(main_class);
    command
}

fn compile(sources: &[(&str, &str)], jars: &[PathBuf]) -> io::Result<PathBuf> {
    let directory = class_directory(sources);
    let stamp = directory.join("compiled");
    if stamp.is_file() {
        return Ok(directory);
    }
    std::fs::create_dir_all(&directory)?;
    let mut files = Vec::new();
    for (name, source) in sources {
        let path = directory.join(name);
        std::fs::write(&path, source)?;
        files.push(path);
    }
    let classpath =
        std::env::join_paths(jars).map_err(|error| io::Error::other(error.to_string()))?;
    let output = std::process::Command::new(java_bin("javac"))
        .arg("-encoding")
        .arg("UTF-8")
        .arg("-proc:none")
        .arg("-cp")
        .arg(&classpath)
        .arg("-d")
        .arg(&directory)
        .args(&files)
        .output()
        .map_err(|error| io::Error::other(format!("javac: {error}")))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "javac exited with {}: {detail}",
            output.status
        )));
    }
    std::fs::write(&stamp, "")?;
    Ok(directory)
}

fn class_directory(sources: &[(&str, &str)]) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (name, source) in sources {
        name.hash(&mut hasher);
        source.hash(&mut hasher);
    }
    std::env::temp_dir().join(format!("krusty-tooling-{:016x}", hasher.finish()))
}

fn gradle_distribution_lib() -> io::Result<PathBuf> {
    let mut found = Vec::new();
    collect_tooling_api(&gradle_user_home().join("wrapper/dists"), 6, &mut found)?;
    found.sort_by_key(|path| tooling_api_version(path));
    let tooling = found
        .pop()
        .ok_or_else(|| io::Error::other("gradle-tooling-api jar is not installed"))?;
    tooling
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("gradle-tooling-api jar has no lib directory"))
}

fn tooling_api_jar(lib: &Path) -> io::Result<PathBuf> {
    jars_in(lib)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_tooling_api_jar)
        })
        .max_by(|left, right| tooling_api_version(left).cmp(&tooling_api_version(right)))
        .ok_or_else(|| io::Error::other("gradle-tooling-api jar is not installed"))
}

fn tooling_api_version(path: &Path) -> Vec<u32> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    name.trim_start_matches("gradle-tooling-api-")
        .trim_end_matches(".jar")
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn is_tooling_api_jar(name: &str) -> bool {
    let prefix = "gradle-tooling-api-";
    name.starts_with(prefix)
        && name.ends_with(".jar")
        && name
            .as_bytes()
            .get(prefix.len())
            .is_some_and(u8::is_ascii_digit)
}

fn maven_classpath_jars() -> io::Result<Vec<PathBuf>> {
    let lib = maven_lib()?;
    let mut jars = jars_in(&lib)?;
    // The embedder container loads plexus-classworlds from Maven's boot directory.
    if let Some(home) = lib.parent() {
        let boot = home.join("boot");
        if boot.is_dir() {
            jars.extend(jars_in(&boot)?);
        }
    }
    Ok(jars)
}

fn maven_lib() -> io::Result<PathBuf> {
    for variable in ["MAVEN_HOME", "M2_HOME"] {
        if let Ok(home) = std::env::var(variable) {
            if let Some(lib) = embedder_lib(Path::new(&home)) {
                return Ok(lib);
            }
        }
    }
    if let Some(lib) = embedder_lib(Path::new("/usr/share/maven")) {
        return Ok(lib);
    }
    // The Maven the plugin jobs install is on PATH. Its lib jars are versioned
    // (`maven-resolver-api-1.9.24.jar`), unlike the unversioned names in a distro package.
    if let Some(path) = std::env::var_os("PATH") {
        if let Some(lib) = embedder_lib_on_path(&path) {
            return Ok(lib);
        }
    }
    Err(io::Error::other("Maven embedder jars are not installed"))
}

fn embedder_lib(home: &Path) -> Option<PathBuf> {
    let lib = home.join("lib");
    has_resolver_api(&lib).then_some(lib)
}

fn embedder_lib_on_path(path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path).find_map(|dir| {
        if !dir.join("mvn").is_file() {
            return None;
        }
        dir.parent().and_then(embedder_lib)
    })
}

fn has_resolver_api(lib: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(lib) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.path().is_file() && is_resolver_api_name(&entry.file_name()))
}

fn is_resolver_api_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name == "maven-resolver-api.jar"
        || (name.starts_with("maven-resolver-api-") && name.ends_with(".jar"))
}

pub(super) fn gradle_user_home() -> PathBuf {
    if let Ok(home) = std::env::var("GRADLE_USER_HOME") {
        return PathBuf::from(home);
    }
    home_dir().join(".gradle")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn jars_in(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut jars = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("jar") {
            jars.push(path);
        }
    }
    if jars.is_empty() {
        return Err(io::Error::other(format!("{} has no jars", dir.display())));
    }
    Ok(jars)
}

fn collect_tooling_api(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) -> io::Result<()> {
    if depth == 0 || !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_tooling_api(&path, depth - 1, found)?;
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_tooling_api_jar)
        {
            found.push(path);
        }
    }
    Ok(())
}

fn java_bin(tool: &str) -> PathBuf {
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let candidate = PathBuf::from(home).join("bin").join(tool);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(tool)
}

fn log_tail(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let start = text.len().saturating_sub(1500);
    format!("\n{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::testing::TempTree;

    #[test]
    fn the_gradle_client_opens_a_tooling_connection() {
        assert!(GRADLE_CLIENT.contains("GradleConnector.newConnector()"));
        assert!(GRADLE_CLIENT.contains(".forProjectDirectory(projectDir)"));
        assert!(GRADLE_CLIENT.contains(".useBuildDistribution()"));
        assert!(GRADLE_CLIENT.contains(".connect()"));
        assert!(GRADLE_CLIENT.contains(".newBuild()"));
        assert!(GRADLE_CLIENT.contains(".forTasks("));
        assert!(GRADLE_CLIENT.contains("CONNECTIONS.get(key)"));
        assert!(!GRADLE_CLIENT.contains("gradlew"));
        assert!(!GRADLE_CLIENT.contains("ProcessBuilder"));
    }

    #[test]
    fn the_maven_client_calls_the_resolver() {
        assert!(MAVEN_CLIENT.contains("system.resolveArtifact("));
        assert!(MAVEN_CLIENT.contains("new ArtifactRequest("));
        assert!(MAVEN_CLIENT.contains("system.resolveDependencies("));
        assert!(MAVEN_CLIENT.contains("new DependencyRequest("));
        assert!(MAVEN_CLIENT.contains("Maven3XServerEmbedder.resolveArtifacts"));
        assert!(MAVEN_CLIENT.contains("DefaultRepositorySystemSessionFactory"));
        assert!(MAVEN_CLIENT.contains("newRepositorySession"));
        assert!(MAVEN_CLIENT.contains("populateFromSettings"));
        assert!(MAVEN_CLIENT.contains("RepositoryUtils.toRepos"));
        assert!(!MAVEN_CLIENT.contains("MavenRepositorySystemUtils.newSession"));
        assert!(!MAVEN_CLIENT.contains("DefaultMirrorSelector"));
        assert!(!MAVEN_CLIENT.contains("ProcessBuilder"));
        assert!(!MAVEN_CLIENT.contains("mvn"));
        assert!(!MAVEN_CLIENT.contains("dependency:resolve"));
        assert!(MAVEN_CLIENT.contains("Protocol.capture()"));
        assert!(GRADLE_CLIENT.contains("Protocol.capture()"));
    }

    #[test]
    fn a_maven_pom_warning_is_not_the_protocol_response() {
        let warning = "[WARNING] The POM for com.hankcs:aho-corasick-double-array-trie:jar:1.2.3 is invalid, transitive dependencies (if any) will not be available, enable debug logging for more details";
        assert!(!is_tool_response(warning));
        assert!(is_tool_response(
            r#"{"status":1,"stdout":"","stderr":"missing"}"#
        ));
    }

    #[test]
    fn an_official_distribution_on_path_is_the_embedder() {
        let tree = TempTree::new("maven-official-home");
        let home = tree.directory("apache-maven");
        let lib = tree.directory("apache-maven/lib");
        let bin = tree.directory("apache-maven/bin");
        assert!(embedder_lib(&home).is_none());
        tree.write("apache-maven/lib/maven-core.jar", "");
        assert!(embedder_lib(&home).is_none());
        tree.write("apache-maven/bin/mvn", "");
        assert!(embedder_lib_on_path(bin.as_os_str()).is_none());
        tree.write("apache-maven/lib/maven-resolver-api-1.9.24.jar", "");
        assert_eq!(embedder_lib(&home).as_deref(), Some(lib.as_path()));
        assert_eq!(
            embedder_lib_on_path(bin.as_os_str()).as_deref(),
            Some(lib.as_path())
        );

        let packaged = tree.directory("share/maven");
        tree.write("share/maven/lib/maven-resolver-api.jar", "");
        assert_eq!(
            embedder_lib(&packaged).as_deref(),
            Some(packaged.join("lib").as_path())
        );
    }

    #[test]
    fn project_repositories_are_normalized_through_the_session() {
        let jars = maven_classpath_jars().expect("Maven embedder jars");
        let directory =
            std::env::temp_dir().join(format!("krusty-maven-normalize-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let client = directory.join("MavenResolverClient.java");
        let protocol = directory.join("Protocol.java");
        let test = directory.join("ProjectRepositoryNormalizationTest.java");
        std::fs::write(&client, MAVEN_CLIENT).unwrap();
        std::fs::write(&protocol, PROTOCOL).unwrap();
        std::fs::write(
            &test,
            include_str!("../../java/krusty/tooling/ProjectRepositoryNormalizationTest.java"),
        )
        .unwrap();
        let compile_path = std::env::join_paths(&jars).unwrap();
        let javac = std::process::Command::new(java_bin("javac"))
            .arg("-encoding")
            .arg("UTF-8")
            .arg("-proc:none")
            .arg("-cp")
            .arg(&compile_path)
            .arg("-d")
            .arg(&directory)
            .arg(&protocol)
            .arg(&client)
            .arg(&test)
            .output()
            .expect("javac");
        assert!(
            javac.status.success(),
            "javac: {}",
            String::from_utf8_lossy(&javac.stderr)
        );
        let mut runtime_jars = vec![directory.clone()];
        runtime_jars.extend(jars);
        let runtime = std::env::join_paths(&runtime_jars).unwrap();
        let java = std::process::Command::new(java_bin("java"))
            .arg("-cp")
            .arg(&runtime)
            .arg("krusty.tooling.ProjectRepositoryNormalizationTest")
            .output()
            .expect("java");
        let _ = std::fs::remove_dir_all(&directory);
        assert!(
            java.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&java.stdout),
            String::from_utf8_lossy(&java.stderr)
        );
    }

    #[test]
    fn the_client_process_is_the_tooling_jvm() {
        let command = client_command(
            Path::new("java"),
            "tooling-api.jar",
            "krusty.tooling.GradleToolingClient",
        );
        assert_eq!(command.get_program().to_string_lossy(), "java");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "-cp".to_string(),
                "tooling-api.jar".to_string(),
                "krusty.tooling.GradleToolingClient".to_string(),
            ]
        );
        assert!(args
            .iter()
            .all(|arg| { !arg.contains("gradlew") && arg != "mvn" && !arg.contains("mvnw") }));
    }

    #[test]
    fn a_newer_tooling_api_outranks_an_older_one() {
        assert!(
            tooling_api_version(Path::new("gradle-tooling-api-9.10.0.jar"))
                > tooling_api_version(Path::new("gradle-tooling-api-9.8.1.jar"))
        );
        assert!(is_tooling_api_jar("gradle-tooling-api-9.8.1.jar"));
        assert!(!is_tooling_api_jar("gradle-tooling-api-builders-9.8.1.jar"));
    }
}
