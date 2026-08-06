use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use krusty::source::SourceKind;
use krusty_lsp::{
    detect, resolve_jdk, AnalysisWorker, DocumentAnalysis, DumpResult, DumpTarget, JdkRequest,
    LibraryRef, LspOptions, MaterializedDefinition, ProcessRunner, ProjectFeedback,
    ProjectMessageKind, ProjectModel, ProjectSources, ProjectSync, ProviderKind, RefreshOutcome,
    SystemEnvironment,
};

#[cfg(unix)]
const ORPHAN_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const WORKER_RECONFIGURE_RETRY_INITIAL_MS: u64 = 1_000;
const WORKER_RECONFIGURE_RETRY_MAX_MS: u64 = 30_000;
const MAX_RETAINED_SUPPORT_DOCUMENTS: usize = 32 * 1024;
/// Dev-mode replay inputs are full compiler source sets, unlike the compact query snapshots kept by
/// normal LSP sessions. Reuse the supervisor's global retained-analysis ceiling so a many-module
/// workspace cannot multiply the same open texts into an unbounded second in-memory workspace.
const MAX_RETAINED_DUMP_INPUT_BYTES: usize = krusty_lsp::MAX_RETAINED_ANALYSIS_BYTES;

fn is_java_source_path(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("java")
}

fn analysis_remains_pending(kind: io::ErrorKind) -> bool {
    matches!(kind, io::ErrorKind::TimedOut | io::ErrorKind::Interrupted)
}

fn finish_analysis(
    analysis_pending: &mut bool,
    result: io::Result<Vec<DocumentAnalysis>>,
    document_count: usize,
) -> Vec<DocumentAnalysis> {
    match result {
        Ok(analysis) => {
            *analysis_pending = false;
            analysis
        }
        Err(error) if analysis_remains_pending(error.kind()) => {
            *analysis_pending = true;
            eprintln!("krusty-lsp: {error}; source analysis remains pending");
            Vec::new()
        }
        Err(error) => {
            *analysis_pending = false;
            (0..document_count)
                .map(|_| {
                    DocumentAnalysis::with_diagnostics(vec![krusty::diag::Diagnostic {
                        span: krusty::diag::Span::new(0, 0),
                        editor_span: None,
                        identity: None,
                        severity: krusty::diag::Severity::Error,
                        kind: krusty::diag::DiagnosticKind::Compiler,
                        msg: format!("analysis worker failed: {error}"),
                        file: 0,
                    }])
                })
                .collect()
        }
    }
}

fn run_cache_command(args: &[String]) {
    let (all, root) = parse_cache_command(args).unwrap_or_else(|error| {
        eprintln!("krusty-lsp: {error}");
        std::process::exit(2);
    });
    let root = root.unwrap_or_else(|| {
        krusty_lsp::deps_cache::default_cache_root(&|key| std::env::var(key).ok())
    });
    match krusty_lsp::deps_cache::clean(&root, all) {
        Ok(freed) => println!("krusty-lsp: freed {freed} bytes from {}", root.display()),
        Err(error) => {
            eprintln!("krusty-lsp: cache clean failed: {error}");
            std::process::exit(1);
        }
    }
}

fn parse_cache_command(args: &[String]) -> Result<(bool, Option<PathBuf>), String> {
    if args.first().map(String::as_str) != Some("clean") {
        return Err("usage: cache clean [--all] [-deps-cache-dir <dir>]".to_string());
    }
    let mut all = false;
    let mut root = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--all" => {
                all = true;
                index += 1;
            }
            "-deps-cache-dir" => {
                let path = args
                    .get(index + 1)
                    .filter(|path| !path.starts_with('-'))
                    .ok_or_else(|| "-deps-cache-dir requires a value".to_string())?;
                root = Some(PathBuf::from(path));
                index += 2;
            }
            option => return Err(format!("unknown cache option '{option}'")),
        }
    }
    Ok((all, root))
}

/// Remove the private worker-mode marker and the spawning server's PID from an argument vector.
///
/// The PID is positional and mandatory because this is an internal exec protocol, not a user-facing
/// option. Keeping it next to the marker also prevents either value from reaching `LspOptions`.
fn take_worker_parent(arguments: &mut Vec<String>) -> Result<Option<u32>, String> {
    let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--analysis-worker")
    else {
        return Ok(None);
    };
    arguments.remove(index);
    let parent = arguments
        .get(index)
        .ok_or_else(|| "--analysis-worker requires the server PID".to_string())?
        .parse::<u32>()
        .map_err(|_| "--analysis-worker server PID must be an unsigned integer".to_string())?;
    arguments.remove(index);
    Ok(Some(parent))
}

/// Terminate the worker once its server is gone.
///
/// The worker normally stops when the server closes its stdin, but a worker busy in
/// analysis never reaches the next read and would survive as an orphan burning a core.
/// Watching for reparenting catches that case without touching the analysis path. The
/// expected PID comes from the spawning server, closing the race where the server exits
/// before the worker can sample its current parent.
#[cfg(unix)]
fn exit_when_orphaned(server: u32) {
    use std::os::unix::process::parent_id;

    std::thread::spawn(move || loop {
        // Check before sleeping so a worker whose server died during exec exits promptly instead
        // of adopting the reaper as its baseline or doing unnecessary compiler initialization.
        if parent_id() != server {
            std::process::exit(0);
        }
        std::thread::sleep(ORPHAN_CHECK_INTERVAL);
    });
}

#[cfg(not(unix))]
fn exit_when_orphaned(_server: u32) {}

/// Where rendered dependency sources are written, from the configured directory or the XDG default.
fn deps_cache_root(options: &LspOptions) -> std::path::PathBuf {
    options
        .deps_cache_dir()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| {
            krusty_lsp::deps_cache::default_cache_root(&|key| std::env::var(key).ok())
        })
}

fn main() {
    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("cache") {
        run_cache_command(&arguments[1..]);
        return;
    }
    let worker_parent = take_worker_parent(&mut arguments).unwrap_or_else(|error| {
        eprintln!("krusty-lsp: {error}");
        std::process::exit(2);
    });
    if let Some(server) = worker_parent {
        exit_when_orphaned(server);
        let stdin = io::stdin();
        let stdout = io::stdout();
        if let Err(error) =
            krusty_lsp::run_configured_analysis_worker(&mut stdin.lock(), &mut stdout.lock())
        {
            eprintln!("krusty-lsp worker: {error}");
            std::process::exit(1);
        }
        return;
    }

    // Worker mode is an internal framed protocol, not a second invocation of the server CLI. Parse
    // user-facing options only in the supervisor: the child receives its complete launch classpath in
    // the first frame and every analysis-specific setting in the request that uses it.
    let options = LspOptions::parse(arguments).unwrap_or_else(|error| {
        eprintln!("krusty-lsp: {error}");
        std::process::exit(2);
    });

    let worker = AnalysisWorker::spawn(
        std::env::current_exe().expect("locate krusty-lsp executable"),
        options.effective_classpath(),
    )
    .unwrap_or_else(|error| {
        eprintln!("krusty-lsp: cannot start analysis worker: {error}");
        std::process::exit(1);
    });

    let cache_root = options
        .deps_cache_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            krusty_lsp::deps_cache::default_cache_root(&|key| std::env::var(key).ok())
        });
    let max_age_days = options.deps_cache_max_age_days();
    let max_bytes = options.deps_cache_max_bytes();
    std::thread::spawn(move || {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let _ = krusty_lsp::deps_cache::gc(&cache_root, max_age_days, max_bytes, now_secs);
    });

    let dev = options.dev();
    let host = WorkerHost::new(worker, options);
    match krusty_lsp::run_stdio_connection_async(host, dev) {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("krusty-lsp: {error}");
            std::process::exit(1);
        }
    }
}

/// One analysis call's inputs, exactly as the worker received them.
///
/// Every field changes what the dump sees. The kinds keep Java and script documents out of the
/// Kotlin parser, the Java sources keep the group's stub overlay, and the language arguments and
/// classpath keep the module's own compiler configuration — a dump analyzed against the launch
/// classpath would report unresolved types for a file the editor shows as clean.
struct AnalysisPayload<'a> {
    sources: &'a [String],
    source_kinds: &'a [SourceKind],
    /// Parallel to `sources`. Empty for a slot whose text was blanked out because the document
    /// belongs to another analysis group, so such a slot never matches a dump request.
    uris: &'a [String],
    result_count: usize,
    inferred_count: usize,
    java_sources: &'a [String],
    language_arguments: &'a [String],
    classpath: Option<&'a [PathBuf]>,
}

/// One analysis group's payload, exactly as that group was analyzed.
#[derive(Default)]
struct RetainedGroup {
    sources: Vec<String>,
    source_kinds: Vec<SourceKind>,
    uris: Vec<String>,
    result_count: usize,
    inferred_count: usize,
    java_sources: Vec<String>,
    language_arguments: Vec<String>,
    classpath: Option<Vec<PathBuf>>,
    /// Digest of everything above. The payload is the dump's only input, so two payloads with the
    /// same digest render the same document and a repeat request can reuse the rendered file.
    fingerprint: u64,
}

/// Digest every input a dump is rendered from.
fn retained_group_fingerprint(payload: &AnalysisPayload<'_>) -> u64 {
    let mut fingerprint = DefaultHasher::new();
    payload.sources.hash(&mut fingerprint);
    for kind in payload.source_kinds {
        kind.wire_code().hash(&mut fingerprint);
    }
    payload.uris.hash(&mut fingerprint);
    payload.result_count.hash(&mut fingerprint);
    payload.inferred_count.hash(&mut fingerprint);
    payload.java_sources.hash(&mut fingerprint);
    payload.language_arguments.hash(&mut fingerprint);
    payload.classpath.hash(&mut fingerprint);
    fingerprint.finish()
}

/// Heap bytes that cloning `payload` into a [`RetainedGroup`] is expected to own.
///
/// Count vector elements as well as their variable data: a workspace can contain thousands of
/// empty strings or classpath entries, so summing string lengths alone is not a memory bound. Vec
/// headers and scalar fields live in `RetainedGroup` itself and are covered by its struct size.
fn retained_payload_bytes(payload: &AnalysisPayload<'_>) -> usize {
    fn strings(values: &[String]) -> usize {
        std::mem::size_of_val(values).saturating_add(
            values
                .iter()
                .fold(0usize, |bytes, value| bytes.saturating_add(value.len())),
        )
    }
    fn paths(values: &[PathBuf]) -> usize {
        std::mem::size_of_val(values).saturating_add(values.iter().fold(0usize, |bytes, value| {
            bytes.saturating_add(value.as_os_str().as_encoded_bytes().len())
        }))
    }

    std::mem::size_of::<RetainedGroup>()
        .saturating_add(strings(payload.sources))
        .saturating_add(std::mem::size_of_val(payload.source_kinds))
        .saturating_add(strings(payload.uris))
        .saturating_add(strings(payload.java_sources))
        .saturating_add(strings(payload.language_arguments))
        .saturating_add(payload.classpath.map_or(0, paths))
}

impl RetainedGroup {
    /// Replay this group as a dump of slot `target`.
    ///
    /// `language_arguments` is always `Some`: the analysis call derives its features from the
    /// module's arguments even when that list is empty, so falling back to the worker's session
    /// features here would dump under a different feature set than the editor analyzed under.
    fn dump_target<'a>(
        &'a self,
        target: usize,
        label: &'a str,
        cache_key: &'a str,
        cache_root: &'a Path,
    ) -> DumpTarget<'a> {
        DumpTarget {
            sources: &self.sources,
            source_kinds: &self.source_kinds,
            target,
            label,
            cache_key,
            cache_root,
            result_count: self.result_count,
            inferred_count: self.inferred_count,
            java_sources: &self.java_sources,
            language_arguments: Some(&self.language_arguments),
            classpath: self.classpath.as_deref(),
        }
    }
}

/// Every group the latest analysis pass covered, kept only under `--dev` so a dump request can
/// replay exactly the source set and configuration the session analyzed.
///
/// One entry per admitted group, not one entry in total: a pass walks every group, and a single
/// retained payload would leave every module but the last one permanently undumpable. Groups are
/// admitted in traversal order up to one global byte budget. This matters because each group may
/// repeat open and support texts from its dependencies; merely dropping superseded passes does not
/// bound that multiplication within one many-module pass.
#[derive(Default)]
struct RetainedAnalysis {
    groups: Vec<RetainedGroup>,
    retained_bytes: usize,
}

impl RetainedAnalysis {
    /// Drop the previous pass's payloads. Cheap and unconditional: outside dev mode nothing was
    /// ever recorded, so this clears an empty vector and allocates nothing.
    fn begin_pass(&mut self) {
        self.groups.clear();
        self.retained_bytes = 0;
    }

    /// Retain one group's payload, or retain nothing at all when dev mode is off.
    ///
    /// Callers additionally skip building the payload outside dev mode — copying every source text
    /// on each keystroke is the cost this gate exists to avoid — but the check lives here so
    /// retention cannot be reached another way.
    fn record(&mut self, dev: bool, payload: &AnalysisPayload<'_>) -> bool {
        self.record_with_budget(dev, payload, MAX_RETAINED_DUMP_INPUT_BYTES)
    }

    /// Budget-parameterized implementation so the invariant can be tested with tiny fixtures
    /// instead of allocating tens of MiB in a unit test.
    fn record_with_budget(
        &mut self,
        dev: bool,
        payload: &AnalysisPayload<'_>,
        max_bytes: usize,
    ) -> bool {
        if !dev {
            return false;
        }
        let retained_bytes = retained_payload_bytes(payload);
        if retained_bytes > max_bytes.saturating_sub(self.retained_bytes) {
            return false;
        }
        self.groups.push(RetainedGroup {
            sources: payload.sources.to_vec(),
            source_kinds: payload.source_kinds.to_vec(),
            uris: payload.uris.to_vec(),
            result_count: payload.result_count,
            inferred_count: payload.inferred_count,
            java_sources: payload.java_sources.to_vec(),
            language_arguments: payload.language_arguments.to_vec(),
            classpath: payload.classpath.map(<[PathBuf]>::to_vec),
            fingerprint: retained_group_fingerprint(payload),
        });
        self.retained_bytes = self.retained_bytes.saturating_add(retained_bytes);
        true
    }

    /// The retained group and slot holding `uri`, if the latest pass covered it.
    ///
    /// A URI is dumpable in at most one group: `project_group_uris` blanks every slot the group
    /// does not own, so support copies of another module's file never claim it.
    fn locate(&self, uri: &str) -> Option<(&RetainedGroup, usize)> {
        if uri.is_empty() {
            return None;
        }
        self.groups.iter().find_map(|group| {
            group
                .uris
                .iter()
                .position(|candidate| candidate == uri)
                .map(|slot| (group, slot))
        })
    }
}

/// Bounded so a session cycling through files cannot grow the reuse list without limit. Small on
/// purpose: an entry only serves repeat requests on a document whose analysis has not moved.
const MAX_RENDERED_DUMPS: usize = 16;

struct RenderedDump {
    uri: String,
    fingerprint: u64,
    path: PathBuf,
}

/// Documents already rendered this session, keyed by the file and the payload it was rendered from.
///
/// A code action is not one deliberate user gesture: clients refresh code actions whenever the
/// cursor settles — Zed's inline indicator, VS Code's lightbulb — and every refresh would otherwise
/// re-parse, re-check and re-lower the whole module group and rewrite a large Markdown file,
/// serially on the thread that also serves diagnostics, completion and hover. A repeat request on
/// an unchanged document costs one hash comparison instead.
///
/// Keyed by the payload rather than by the document version deliberately. The payload is what the
/// document is rendered from, and it can lag the buffer; a version key would pin a dump rendered
/// from pre-edit state and keep serving it after the analysis had caught up.
#[derive(Default)]
struct RenderedDumps {
    entries: Vec<RenderedDump>,
}

impl RenderedDumps {
    /// The file already rendered for `uri` from this exact payload, if there is one.
    fn lookup(&self, uri: &str, fingerprint: u64) -> Option<&Path> {
        self.entries
            .iter()
            .find(|entry| entry.fingerprint == fingerprint && entry.uri == uri)
            .map(|entry| entry.path.as_path())
    }

    /// Note what a render produced. One entry per URI: an entry whose payload has been superseded
    /// can never be reused, so it is replaced rather than kept.
    fn record(&mut self, uri: &str, fingerprint: u64, path: &Path) {
        self.entries.retain(|entry| entry.uri != uri);
        if self.entries.len() >= MAX_RENDERED_DUMPS {
            self.entries.remove(0);
        }
        self.entries.push(RenderedDump {
            uri: uri.to_string(),
            fingerprint,
            path: path.to_path_buf(),
        });
    }
}

/// Workspace-relative presentation label for a document URI.
///
/// The cache key is the full URI and is hashed separately. A non-file label keeps only its scheme
/// and last path component, so a query string carrying editor/session data is never copied into the
/// dump body merely to provide a heading.
fn workspace_relative_label(root: Option<&Path>, uri: &str) -> String {
    let label = if let Some(path) = krusty_lsp::uri::file_uri_to_path(uri) {
        if let Some(relative) = root.and_then(|root| path.strip_prefix(root).ok()) {
            relative.to_string_lossy().into_owned()
        } else {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "<external file>".to_string())
        }
    } else if let Ok(uri) = url::Url::parse(uri) {
        let component = uri
            .path()
            .rsplit('/')
            .find(|component| !component.is_empty());
        component.map_or_else(
            || format!("{}:<document>", uri.scheme()),
            |component| format!("{}:{component}", uri.scheme()),
        )
    } else {
        "<document>".to_string()
    };
    bounded_dump_label(label)
}

fn bounded_dump_label(mut label: String) -> String {
    const MAX_LABEL_BYTES: usize = 1024;
    if label.len() <= MAX_LABEL_BYTES {
        return label;
    }
    let mut end = MAX_LABEL_BYTES;
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    label.truncate(end);
    label.push('…');
    label
}

struct WorkerHost {
    worker: AnalysisWorker,
    options: LspOptions,
    runner: ProcessRunner,
    sync: Option<ProjectSync>,
    clock: Instant,
    root: Option<PathBuf>,
    jdk_warning_shown: bool,
    /// Set when the source inventory was cut short — an unreadable root or the retention
    /// ceiling — so the shortfall is reported rather than looking like a fully indexed workspace.
    truncated_inventory: bool,
    /// One model generation's URI inventory. Both priority tiers derive from this snapshot; walking
    /// every source root once for the neighbourhood and again for the sweep doubled cold indexing
    /// work, while rebuilding it after every keystroke could block the interactive engine thread.
    workspace_inventory: Option<Vec<String>>,
    project_sources: ProjectSources,
    /// Background chunks use the same module/source-set algorithm but a separate discovery cache,
    /// so hours of indexing cannot evict the open document's hot support-source inventory.
    index_project_sources: ProjectSources,
    analysis_cache: Vec<CachedProjectAnalysis>,
    analysis_pending: bool,
    platform_classpath: Vec<PathBuf>,
    worker_reconfigure_retry_at_ms: Option<u64>,
    worker_reconfigure_retry_backoff_ms: u64,
    retained: RetainedAnalysis,
    rendered_dumps: RenderedDumps,
    /// Engine-installed reporter for the workspace file-tree scan. `None` outside the engine
    /// (tests, one-shot paths), where scan progress has no client to render on.
    scan_reporter: Option<krusty_lsp::ScanReporter>,
}

impl WorkerHost {
    fn new(mut worker: AnalysisWorker, options: LspOptions) -> Self {
        worker.set_language_features(options.language_features());
        let platform_classpath =
            krusty_lsp::effective_platform_classpath(options.jdk_home(), options.no_jdk());
        Self {
            worker,
            options,
            runner: ProcessRunner,
            sync: None,
            clock: Instant::now(),
            root: None,
            jdk_warning_shown: false,
            truncated_inventory: false,
            workspace_inventory: None,
            project_sources: ProjectSources::default(),
            index_project_sources: ProjectSources::default(),
            analysis_cache: Vec::new(),
            analysis_pending: false,
            platform_classpath,
            worker_reconfigure_retry_at_ms: None,
            worker_reconfigure_retry_backoff_ms: 0,
            retained: RetainedAnalysis::default(),
            rendered_dumps: RenderedDumps::default(),
            scan_reporter: None,
        }
    }

    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    fn ensure_workspace_inventory(&mut self) {
        if self.workspace_inventory.is_some() {
            return;
        }
        let Some(model) = self
            .sync
            .as_ref()
            .and_then(ProjectSync::snapshot)
            .map(|snapshot| snapshot.model())
        else {
            self.workspace_inventory = Some(Vec::new());
            return;
        };
        // Taken rather than borrowed: the model borrows `self.sync`, so the reporter cannot be
        // reached through `self` while the walk runs. It goes back immediately after.
        let mut reporter = self.scan_reporter.take();
        if let Some(reporter) = reporter.as_mut() {
            reporter(krusty_lsp::ScanProgress::Started);
        }
        let started = Instant::now();
        let mut ignore = |_| {};
        let progress: &mut dyn FnMut(krusty_lsp::ScanProgress) = match reporter.as_mut() {
            Some(reporter) => reporter,
            None => &mut ignore,
        };
        let (sources, mut truncated) = krusty_lsp::project::workspace_sources(model, progress);
        if let Some(reporter) = reporter.as_mut() {
            reporter(krusty_lsp::ScanProgress::Finished {
                files: sources.len() as u64,
                millis: started.elapsed().as_millis() as u64,
            });
        }
        self.scan_reporter = reporter;
        let mut retained_bytes = 0usize;
        let mut uris = Vec::new();
        for path in sources {
            let Some(uri) = krusty_lsp::uri::path_to_file_uri(&path) else {
                continue;
            };
            let uri_bytes = krusty_lsp::workspace_index_uri_bytes(&uri);
            if uris.len() >= krusty_lsp::MAX_WORKSPACE_INDEX_FILES
                || uri_bytes
                    > krusty_lsp::MAX_WORKSPACE_INDEX_URI_BYTES.saturating_sub(retained_bytes)
            {
                truncated = true;
                break;
            }
            retained_bytes = retained_bytes.saturating_add(uri_bytes);
            uris.push(uri);
        }
        self.truncated_inventory |= truncated;
        self.workspace_inventory = Some(uris);
    }

    fn configure(&mut self) -> ProjectFeedback {
        let previous_snapshot = self.sync.as_ref().and_then(ProjectSync::snapshot).cloned();
        let Some(sync) = self.sync.as_mut() else {
            return ProjectFeedback::default();
        };
        match sync.refresh(&self.runner) {
            RefreshOutcome::Unchanged => ProjectFeedback::default(),
            RefreshOutcome::Updated => {
                self.project_sources.invalidate();
                self.index_project_sources.invalidate();
                self.analysis_cache.clear();
                self.workspace_inventory = None;
                self.truncated_inventory = false;
                let (classpath, jdk_home) = Self::launch_from(sync, &self.options, &self.runner);
                let mut language_features = sync.project_language_features();
                self.options.apply_language_features(&mut language_features);
                let logs = Self::describe_model(
                    sync.kind(),
                    sync.model().map_or(0, |model| model.modules.len()),
                    &classpath,
                    jdk_home.as_deref(),
                );
                if let Err(error) =
                    self.worker
                        .reconfigure(&classpath, jdk_home.as_deref(), self.options.no_jdk())
                {
                    sync.rollback_snapshot(previous_snapshot);
                    let (retry_at, backoff) = next_worker_reconfigure_retry(
                        self.now_ms(),
                        self.worker_reconfigure_retry_backoff_ms,
                    );
                    self.worker_reconfigure_retry_at_ms = Some(retry_at);
                    self.worker_reconfigure_retry_backoff_ms = backoff;
                    return ProjectFeedback {
                        reanalyze: false,
                        message: Some((
                            ProjectMessageKind::Error,
                            format!("krusty: could not restart analysis worker: {error}"),
                        )),
                        logs,
                    };
                }
                self.worker_reconfigure_retry_at_ms = None;
                self.worker_reconfigure_retry_backoff_ms = 0;
                self.platform_classpath = krusty_lsp::effective_platform_classpath(
                    jdk_home.as_deref(),
                    self.options.no_jdk(),
                );
                self.worker.set_language_features(language_features);
                ProjectFeedback {
                    reanalyze: true,
                    message: self.jdk_warning(jdk_home.is_some()),
                    logs,
                }
            }
            RefreshOutcome::Failed {
                error,
                model_retained,
            } => {
                if self.worker_reconfigure_retry_backoff_ms > 0
                    && self.worker_reconfigure_retry_at_ms.is_none()
                {
                    let (retry_at, backoff) = next_worker_reconfigure_retry(
                        self.now_ms(),
                        self.worker_reconfigure_retry_backoff_ms,
                    );
                    self.worker_reconfigure_retry_at_ms = Some(retry_at);
                    self.worker_reconfigure_retry_backoff_ms = backoff;
                }
                let kind = if model_retained {
                    ProjectMessageKind::Warning
                } else {
                    ProjectMessageKind::Error
                };
                let detail = format!("krusty: project sync failed: {error}");
                ProjectFeedback {
                    reanalyze: false,
                    message: Some((kind, detail.clone())),
                    logs: vec![detail],
                }
            }
        }
    }

    fn describe_model(
        kind: ProviderKind,
        modules: usize,
        classpath: &[PathBuf],
        jdk_home: Option<&Path>,
    ) -> Vec<String> {
        const MAX_LISTED: usize = 60;
        let mut logs = vec![format!(
            "krusty: {} — {modules} module(s), {} classpath entr{}",
            kind.as_str(),
            classpath.len(),
            if classpath.len() == 1 { "y" } else { "ies" },
        )];
        logs.push(format!(
            "krusty: JDK = {}",
            jdk_home.map_or_else(|| "none".to_string(), |home| home.display().to_string()),
        ));
        if !classpath.is_empty() {
            let mut listing = String::from("krusty: classpath:");
            for entry in classpath.iter().take(MAX_LISTED) {
                listing.push_str("\n  ");
                listing.push_str(&entry.to_string_lossy());
            }
            if classpath.len() > MAX_LISTED {
                listing.push_str(&format!("\n  … {} more", classpath.len() - MAX_LISTED));
            }
            logs.push(listing);
        }
        logs
    }

    fn launch_from(
        sync: &ProjectSync,
        options: &LspOptions,
        runner: &ProcessRunner,
    ) -> (Vec<PathBuf>, Option<PathBuf>) {
        let classpath = sync.project_classpath();
        if options.no_jdk() {
            return (classpath, None);
        }
        let toolchain = sync.model().and_then(|model| model.jdk_home.clone());
        let jdk = resolve_jdk(
            &SystemEnvironment,
            runner,
            &JdkRequest {
                explicit: options.jdk_home(),
                toolchain: toolchain.as_deref(),
                jvm_target: sync.jvm_target(),
            },
        );
        (classpath, jdk.map(|jdk| jdk.home))
    }

    fn jdk_warning(&mut self, jdk_found: bool) -> Option<(ProjectMessageKind, String)> {
        if jdk_found || self.options.no_jdk() || self.jdk_warning_shown {
            return None;
        }
        self.jdk_warning_shown = true;
        Some((
            ProjectMessageKind::Warning,
            "krusty: no JDK found — set -jdk-home, JAVA_HOME, or install a JDK on PATH; \
             analysis will be limited until then"
                .to_string(),
        ))
    }
}

/// Bytes one index chunk may read. Mirrors the open-document budget in spirit: a count of files is
/// not a memory bound.
const MAX_INDEX_CHUNK_BYTES: usize = 8 * 1024 * 1024;

impl krusty_lsp::Analysis for WorkerHost {
    fn index_workspace_files(&mut self, uris: &[&str]) -> krusty_lsp::IndexOutcome {
        let mut budget = MAX_INDEX_CHUNK_BYTES;
        let readable: Vec<(String, String)> = uris
            .iter()
            .filter_map(|uri| {
                let path = krusty_lsp::uri::file_uri_to_path(uri)?;
                // Reject a known-oversized file before allocating its contents. Rechecking the
                // actual string length after the read handles a file that grows between metadata
                // and read without charging an inaccurate size.
                let metadata_bytes = usize::try_from(std::fs::metadata(&path).ok()?.len()).ok()?;
                if metadata_bytes > budget {
                    return None;
                }
                let text = std::fs::read_to_string(path).ok()?;
                // The open-document path is byte-bounded; indexing has to be too, or a generated
                // multi-hundred-megabyte source would sit in memory twice per chunk.
                budget = budget.checked_sub(text.len())?;
                Some(((*uri).to_string(), text))
            })
            .collect();
        if readable.is_empty() {
            // No worker call was needed, but every URI was conclusively absent, unreadable, or
            // outside the read budget. Treating this as infrastructure failure would make a
            // one-file deletion retain stale diagnostics forever.
            return krusty_lsp::IndexOutcome {
                files: Vec::new(),
                conclusive: true,
            };
        }
        let documents: Vec<(&str, &str)> = readable
            .iter()
            .map(|(uri, text)| (uri.as_str(), text.as_str()))
            .collect();
        let indexed_uris: Vec<&str> = documents.iter().map(|(uri, _)| *uri).collect();
        // Use the same module grouping, source visibility, language flags, and classpath selection
        // as interactive analysis. A raw `analyze(&texts)` call loses every one of those origins
        // and publishes false unresolved-reference diagnostics for otherwise valid workspace files.
        //
        // Indexing still must not evict or populate the interactive cache. Temporarily replacing
        // that cache lets the shared project-analysis path stay the single semantic implementation
        // while keeping background chunks invisible to the next keystroke's hot state.
        let interactive_cache = std::mem::take(&mut self.analysis_cache);
        std::mem::swap(&mut self.project_sources, &mut self.index_project_sources);
        let (analyses, _support) = self.analyze_open_documents(&documents, &indexed_uris);
        std::mem::swap(&mut self.project_sources, &mut self.index_project_sources);
        self.analysis_cache = interactive_cache;
        // A short result means the worker did not answer; report it as inconclusive so the store
        // keeps what it already has rather than treating the gap as deletions.
        let conclusive = analyses.len() == readable.len();
        let files = analyses
            .into_iter()
            .zip(readable)
            .map(|(analysis, (uri, text))| krusty_lsp::IndexedFile {
                uri,
                diagnostics: analysis.diagnostics,
                text_hash: krusty_lsp::workspace_text_hash(&text),
                text,
            })
            .collect();
        krusty_lsp::IndexOutcome { files, conclusive }
    }

    fn neighborhood_index_candidates(&mut self, open_uris: &[&str]) -> Vec<String> {
        self.ensure_workspace_inventory();
        let Some(snapshot) = self.sync.as_ref().and_then(ProjectSync::snapshot) else {
            return Vec::new();
        };
        // The immutable snapshot owns both module relations and the component-indexed source-root
        // relation. Keep every classification decision on that shared relation; unwrapping the
        // model here would silently restore an O(files × roots) scan during neighborhood indexing.
        let open_modules: std::collections::HashSet<usize> = open_uris
            .iter()
            .filter_map(|uri| krusty_lsp::uri::file_uri_to_path(uri))
            .filter_map(|path| snapshot.module_index_for_source(&path))
            .collect();
        if open_modules.is_empty() {
            return Vec::new();
        }
        self.workspace_inventory
            .as_ref()
            .into_iter()
            .flatten()
            .filter(|uri| {
                let Some(path) = krusty_lsp::uri::file_uri_to_path(uri) else {
                    return false;
                };
                snapshot
                    .module_index_for_source(&path)
                    .is_some_and(|module| open_modules.contains(&module))
            })
            .cloned()
            .collect()
    }

    /// Candidates come from the project model's own source inventory. Walking the tree separately
    /// here would be a second, divergent definition of what counts as a workspace source.
    fn workspace_index_candidates(&mut self) -> Vec<String> {
        self.ensure_workspace_inventory();
        self.workspace_inventory.clone().unwrap_or_default()
    }

    fn workspace_index_incomplete(&self) -> bool {
        self.truncated_inventory
    }

    fn set_scan_reporter(&mut self, reporter: krusty_lsp::ScanReporter) {
        self.scan_reporter = Some(reporter);
    }

    fn document_admission(&self) -> krusty_lsp::DocumentAdmission {
        self.sync
            .as_ref()
            .and_then(ProjectSync::snapshot)
            .map(krusty_lsp::DocumentAdmission::for_snapshot)
            .unwrap_or_default()
    }

    fn analysis_ready(&self) -> bool {
        self.sync.as_ref().and_then(ProjectSync::model).is_some()
    }

    fn analysis_pending(&self) -> bool {
        self.analysis_pending
    }

    fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
        let result = self.worker.analyze(sources);
        finish_analysis(&mut self.analysis_pending, result, sources.len())
    }

    /// Class names from the project's own classpath, read once per project model.
    ///
    /// Built here rather than in the worker because it needs only the jar catalogues -- the entry
    /// names in each archive -- not decoded classes, and shipping the list over the worker wire
    /// would cost more than reading it. Each jar's listing is cached, which is where most of the
    /// cost went: 442 ms of 706 ms over 150 jars, against 11 ms to read it back.
    fn dependency_index(&mut self) -> krusty_lsp::DependencySymbolIndex {
        let Some(sync) = self.sync.as_ref() else {
            return krusty_lsp::DependencySymbolIndex::default();
        };
        let mut entries = sync.dependency_classpath();
        entries.extend(self.platform_classpath.iter().cloned());
        if entries.is_empty() {
            return krusty_lsp::DependencySymbolIndex::default();
        }
        // Raw class listings are auxiliary entries in the same managed cache as rendered sources,
        // so age/size GC, locking, and ordinary `cache clean` cover both.
        let cache_root = deps_cache_root(&self.options);
        krusty_lsp::DependencySymbolIndex::from_cached_classpath(&entries, &cache_root)
    }

    /// Write out the classes a query is about to return, through the worker that already holds a
    /// decoded classpath. Off the request path: the query it serves was answered without them.
    fn locate_dependencies(
        &mut self,
        candidates: Vec<krusty_lsp::DependencyCandidate>,
    ) -> Vec<krusty_lsp::LocatedDependency> {
        let cache_root = deps_cache_root(&self.options);
        let use_sources = self.options.deps_sources_enabled();
        krusty_lsp::locate_dependencies_with(&cache_root, candidates, |candidate| {
            let reference = LibraryRef {
                fqn: candidate.internal.clone(),
                member_name: String::new(),
                member_desc: String::new(),
            };
            self.worker
                .materialize_library_definition(&reference, use_sources)
                .ok()
                .flatten()
        })
    }

    fn materialize_library_definition(
        &mut self,
        reference: &LibraryRef,
    ) -> Option<MaterializedDefinition> {
        let (text, span) = self
            .worker
            .materialize_library_definition(reference, self.options.deps_sources_enabled())
            .ok()
            .flatten()?;
        let cache_root = deps_cache_root(&self.options);
        let path = krusty_lsp::deps_cache::store(&cache_root, &reference.fqn, &text).ok()?;
        Some(MaterializedDefinition {
            path,
            text,
            lo: span.lo,
            hi: span.hi,
        })
    }

    fn dump(&mut self, uri: &str) -> Option<DumpResult> {
        if !self.options.dev() {
            return None;
        }
        let (group, slot) = self.retained.locate(uri)?;
        let fingerprint = group.fingerprint;
        // Re-rendering costs a full re-parse, re-check and re-lowering of the module group plus a
        // large file write, all on the thread that also serves diagnostics and completion. Nothing
        // about the document has changed since the last render, so nothing about it needs redoing.
        if let Some(path) = self
            .rendered_dumps
            .lookup(uri, fingerprint)
            .filter(|path| path.exists())
        {
            return Some(DumpResult {
                path: path.to_path_buf(),
            });
        }
        let label = workspace_relative_label(self.root.as_deref(), uri);
        let cache_root = self
            .options
            .deps_cache_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| {
                krusty_lsp::deps_cache::default_cache_root(&|key| std::env::var(key).ok())
            });
        let response = self
            .worker
            // `label` is presentation text; the full URI is the stable document identity. Keeping
            // those roles separate prevents two same-named files outside the workspace from
            // sharing a cache entry, while `dump_cache` hashes the URI so it never reaches disk as
            // readable path or class-name text.
            .dump(&group.dump_target(slot, &label, uri, &cache_root))
            .ok()
            .flatten()?;
        self.rendered_dumps.record(uri, fingerprint, &response.path);
        Some(DumpResult {
            path: response.path,
        })
    }

    fn analyze_open_documents(
        &mut self,
        documents: &[(&str, &str)],
        open_uris: &[&str],
    ) -> (Vec<DocumentAnalysis>, Vec<(String, String)>) {
        let module_assignments = project_module_assignments(
            self.sync.as_ref().and_then(ProjectSync::snapshot),
            documents,
        );
        let group_seeds = project_analysis_groups(&module_assignments);
        let module_relations = self.sync.as_ref().and_then(ProjectSync::snapshot);
        let mut analyses = (0..documents.len())
            .map(|_| DocumentAnalysis::empty())
            .collect::<Vec<_>>();
        let mut workspace_symbols = krusty_lsp::WorkspaceSymbolIndex::default();
        self.analysis_pending = false;
        // Retention spans the pass, not one group inside it: every group this loop reaches is
        // recorded, so a file is dumpable whichever module it belongs to.
        self.retained.begin_pass();

        let mut retained_support_bytes = 0usize;
        let mut support_documents = Vec::new();
        let mut support_indices = HashMap::<String, usize>::new();
        let mut next_discarded_support_file = u32::MAX;
        self.analysis_cache.retain(|cached| {
            group_seeds.iter().any(|(module_index, document_indices)| {
                *module_index == cached.module_index && *document_indices == cached.document_indices
            })
        });
        let mut remaining_analysis_bytes = krusty_lsp::MAX_RETAINED_ANALYSIS_BYTES;
        for (module_index, document_indices) in group_seeds {
            let modeled_documents = document_indices
                .iter()
                .map(|&index| documents[index])
                .collect::<Vec<_>>();
            let loaded = match (
                self.sync.as_ref().and_then(ProjectSync::model),
                module_index,
            ) {
                (Some(_), Some(_)) => self
                    .project_sources
                    .load(
                        module_relations.expect("project model relation graph"),
                        &modeled_documents,
                        open_uris,
                        krusty_lsp::MAX_SOURCE_SET_BYTES,
                    )
                    .map(|(sources, inferred_count, java_sources)| {
                        (sources.to_vec(), inferred_count, java_sources)
                    }),
                _ => Ok((Vec::new(), 0, Vec::new())),
            };
            let (mut group_support, mut inferred_support_count, java_sources) = match loaded {
                Ok(loaded) => loaded,
                Err(message) => {
                    fail_project_group(
                        &mut analyses,
                        &mut self.analysis_cache,
                        module_index,
                        &document_indices,
                        &message,
                    );
                    continue;
                }
            };
            let relations = module_relations
                .filter(|snapshot| {
                    !matches!(
                        snapshot.model().kind,
                        ProviderKind::Explicit | ProviderKind::None
                    )
                })
                .and_then(|snapshot| {
                    module_index.and_then(|module_index| snapshot.get(module_index))
                });
            let visible_open_documents = if let Some(relations) = relations {
                let friend_indices = &relations.friends;
                let dependency_indices = relations
                    .dependencies
                    .iter()
                    .copied()
                    .filter(|index| !friend_indices.contains(index))
                    .collect::<Vec<_>>();
                (
                    open_documents_from_modules(friend_indices, documents, &module_assignments),
                    open_documents_from_modules(
                        &dependency_indices,
                        documents,
                        &module_assignments,
                    ),
                )
            } else {
                (Vec::new(), Vec::new())
            };
            let (friend_documents, dependency_documents) = visible_open_documents;
            if !friend_documents.is_empty() {
                group_support.splice(
                    inferred_support_count..inferred_support_count,
                    friend_documents
                        .iter()
                        .map(|(_, uri, source)| (uri.clone(), source.clone())),
                );
                inferred_support_count += friend_documents.len();
            }
            if !dependency_documents.is_empty() {
                group_support.splice(
                    inferred_support_count..inferred_support_count,
                    dependency_documents
                        .iter()
                        .map(|(_, uri, source)| (uri.clone(), source.clone())),
                );
            }

            let group_source_bytes = source_bytes(
                document_indices
                    .iter()
                    .map(|&index| documents[index].1)
                    .chain(group_support.iter().map(|(_, source)| source.as_str()))
                    .chain(java_sources.iter().map(String::as_str)),
            );
            let fits_worker =
                group_source_bytes.is_some_and(|bytes| bytes <= krusty_lsp::MAX_SOURCE_SET_BYTES);
            if !fits_worker {
                fail_project_group(
                    &mut analyses,
                    &mut self.analysis_cache,
                    module_index,
                    &document_indices,
                    &project_source_size_limit_message(),
                );
                continue;
            }
            let remaining_support_entries =
                MAX_RETAINED_SUPPORT_DOCUMENTS.saturating_sub(support_documents.len());
            let (navigation_file_remaps, added_bytes) = register_canonical_support(
                documents,
                &group_support,
                &mut support_documents,
                &mut support_indices,
                krusty_lsp::MAX_SOURCE_SET_BYTES.saturating_sub(retained_support_bytes),
                remaining_support_entries,
                &mut next_discarded_support_file,
            );
            retained_support_bytes += added_bytes;
            let group = ProjectAnalysisGroup {
                module_index,
                document_indices,
                support_documents: group_support,
                inferred_support_count,
                java_sources,
                navigation_file_remaps,
            };
            let inputs = project_group_inputs(documents, &group);
            let fingerprint = project_group_fingerprint(documents, &group);
            // Resolved before the cache lookup because retention has to happen on both arms: a pass
            // that serves this group from cache still analyzed it, and a dump of one of its files
            // must not fail just because no re-analysis was needed. Outside dev mode this stays
            // `None` and costs nothing.
            let mut group_config = self.options.dev().then(|| {
                project_group_compiler_config(
                    self.sync.as_ref().and_then(ProjectSync::model),
                    group.module_index,
                    &self.platform_classpath,
                    &self.options,
                )
            });
            if let Some((classpath, language_arguments)) = group_config.as_ref() {
                let retained_uris = project_group_uris(documents, &group);
                let retained_sources = inputs
                    .iter()
                    .map(|input| input.text.to_string())
                    .collect::<Vec<_>>();
                let retained_kinds = inputs.iter().map(|input| input.kind).collect::<Vec<_>>();
                self.retained.record(
                    true,
                    &AnalysisPayload {
                        sources: &retained_sources,
                        source_kinds: &retained_kinds,
                        uris: &retained_uris,
                        result_count: documents.len(),
                        inferred_count: documents.len() + group.inferred_support_count,
                        java_sources: &group.java_sources,
                        language_arguments,
                        classpath: classpath.as_deref(),
                    },
                );
            }
            let mut selected = if let Some(index) = self.analysis_cache.iter().position(|cached| {
                cached.module_index == group.module_index
                    && cached.fingerprint == fingerprint
                    && cached.document_indices == group.document_indices
            }) {
                let cached = self.analysis_cache.remove(index);
                let selected = cached.analyses.clone();
                self.analysis_cache.push(cached);
                selected
            } else {
                let (classpath, language_arguments) = group_config.take().unwrap_or_else(|| {
                    project_group_compiler_config(
                        self.sync.as_ref().and_then(ProjectSync::model),
                        group.module_index,
                        &self.platform_classpath,
                        &self.options,
                    )
                });
                let result = self.worker.analyze_inputs_prefix_with_config(
                    &inputs,
                    documents.len(),
                    documents.len() + group.inferred_support_count,
                    &group.java_sources,
                    &language_arguments,
                    classpath.as_deref(),
                );
                let cacheable = result.is_ok();
                let mut group_analyses =
                    finish_analysis(&mut self.analysis_pending, result, documents.len());
                if self.analysis_pending {
                    return (Vec::new(), Vec::new());
                }
                if group_analyses.len() != documents.len() {
                    return (Vec::new(), support_documents);
                }
                let retained_file_count = documents.len().saturating_add(support_documents.len());
                for analysis in &mut group_analyses {
                    analysis
                        .remap_navigation_files(&group.navigation_file_remaps, retained_file_count);
                }
                let mut workspace_symbols = krusty_lsp::WorkspaceSymbolIndex::default();
                for analysis in &mut group_analyses {
                    workspace_symbols.merge_from(std::mem::take(&mut analysis.workspace_symbols));
                }
                let implementation_relations = group_analyses
                    .iter_mut()
                    .flat_map(|analysis| std::mem::take(&mut analysis.implementation_relations))
                    .collect::<Vec<_>>();
                let mut selected = group
                    .document_indices
                    .iter()
                    .map(|&index| group_analyses[index].clone())
                    .collect::<Vec<_>>();
                if let Some(first) = selected.first_mut() {
                    first.implementation_relations = implementation_relations;
                    first.workspace_symbols = workspace_symbols;
                }
                if cacheable {
                    self.analysis_cache
                        .retain(|cached| cached.module_index != group.module_index);
                    let retained_bytes = selected
                        .iter()
                        .map(DocumentAnalysis::retained_wire_bytes)
                        .sum::<usize>();
                    retain_analysis_cache_budget(
                        &mut self.analysis_cache,
                        retained_bytes,
                        krusty_lsp::MAX_RETAINED_ANALYSIS_BYTES,
                    );
                    if retained_bytes <= krusty_lsp::MAX_RETAINED_ANALYSIS_BYTES {
                        self.analysis_cache.push(CachedProjectAnalysis {
                            module_index: group.module_index,
                            fingerprint,
                            document_indices: group.document_indices.clone(),
                            analyses: selected.clone(),
                            retained_bytes,
                        });
                    }
                }
                selected
            };
            krusty_lsp::retain_analysis_wire_budget(&mut selected, remaining_analysis_bytes);
            let selected_bytes = selected
                .iter()
                .map(DocumentAnalysis::retained_wire_bytes)
                .sum::<usize>();
            remaining_analysis_bytes = remaining_analysis_bytes.saturating_sub(selected_bytes);
            for analysis in &mut selected {
                workspace_symbols.merge_from(std::mem::take(&mut analysis.workspace_symbols));
            }
            for (&index, analysis) in group.document_indices.iter().zip(selected) {
                analyses[index] = analysis;
            }
        }
        if let Some(first) = analyses.first_mut() {
            first.workspace_symbols = workspace_symbols;
        }
        krusty_lsp::merge_cross_document_implementations(&mut analyses);
        krusty_lsp::retain_analysis_wire_budget(
            &mut analyses,
            krusty_lsp::MAX_RETAINED_ANALYSIS_BYTES,
        );
        (analyses, support_documents)
    }

    fn set_workspace_root(&mut self, root: Option<PathBuf>) -> ProjectFeedback {
        let root = root
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let provider = detect(&root, self.options.explicit_classpath());
        let root_display = root.display().to_string();
        self.root = Some(root);
        self.sync = Some(ProjectSync::new(provider));
        self.project_sources.invalidate();
        self.index_project_sources.invalidate();
        self.analysis_cache.clear();
        self.workspace_inventory = None;
        self.truncated_inventory = false;
        // A root change can change both a module configuration and the workspace-relative heading.
        // Do not reuse a retained payload or rendered path until the replacement analysis pass has
        // established both under the new root.
        self.retained.begin_pass();
        self.rendered_dumps.entries.clear();
        let mut feedback = self.configure();
        feedback
            .logs
            .insert(0, format!("krusty: workspace {root_display}"));
        feedback
    }

    fn watched_globs(&mut self) -> Vec<String> {
        let mut globs = self
            .sync
            .as_ref()
            .map(ProjectSync::watch_globs)
            .unwrap_or_default();
        for extension in krusty::source::SUPPORTED_EXTENSIONS
            .iter()
            .copied()
            .chain(std::iter::once("java"))
        {
            let source_glob = format!("**/*.{extension}");
            if !globs.iter().any(|glob| glob == source_glob.as_str()) {
                globs.push(source_glob);
            }
        }
        globs
    }

    fn note_project_change(&mut self) {
        let now = self.now_ms();
        if let Some(sync) = self.sync.as_mut() {
            sync.note_change(now);
        }
    }

    fn note_watched_file_change(&mut self, uri: &str) -> bool {
        let path = url::Url::parse(uri)
            .ok()
            .and_then(|uri| uri.to_file_path().ok());
        let is_project_change = path.as_ref().is_some_and(|path| {
            self.sync
                .as_ref()
                .is_some_and(|sync| sync.watch_paths().iter().any(|watched| watched == path))
        });
        if is_project_change {
            self.note_project_change();
            return false;
        }
        let is_project_source = path.as_ref().is_some_and(|path| {
            (krusty::source::is_supported_path(path) || is_java_source_path(path))
                && self
                    .sync
                    .as_ref()
                    .and_then(ProjectSync::snapshot)
                    .and_then(|snapshot| snapshot.module_index_for_source(path))
                    .is_some()
        });
        if is_project_source {
            self.project_sources.invalidate();
            self.index_project_sources.invalidate();
            true
        } else {
            self.note_project_change();
            false
        }
    }

    fn project_refresh_due_in(&self) -> Option<Duration> {
        let now = self.now_ms();
        let project_due = self.sync.as_ref().and_then(|sync| sync.refresh_due_in(now));
        let worker_due = self
            .worker_reconfigure_retry_at_ms
            .map(|deadline| deadline.saturating_sub(now));
        match (project_due, worker_due) {
            (Some(project), Some(worker)) => Some(Duration::from_millis(project.min(worker))),
            (Some(project), None) => Some(Duration::from_millis(project)),
            (None, Some(worker)) => Some(Duration::from_millis(worker)),
            (None, None) => None,
        }
    }

    fn refresh_project(&mut self) -> ProjectFeedback {
        let now = self.now_ms();
        let worker_retry_due = self
            .worker_reconfigure_retry_at_ms
            .is_some_and(|deadline| deadline <= now);
        let Some(sync) = self.sync.as_mut() else {
            return ProjectFeedback::default();
        };
        let project_refresh_due = sync.take_due(now);
        if !project_refresh_due && !worker_retry_due {
            return ProjectFeedback::default();
        }
        if project_refresh_due {
            if let Some(root) = &self.root {
                sync.update_provider(detect(root, self.options.explicit_classpath()));
            }
        }
        if worker_retry_due {
            self.worker_reconfigure_retry_at_ms = None;
        }
        self.configure()
    }
}

fn next_worker_reconfigure_retry(now_ms: u64, previous_backoff_ms: u64) -> (u64, u64) {
    let backoff = if previous_backoff_ms == 0 {
        WORKER_RECONFIGURE_RETRY_INITIAL_MS
    } else {
        previous_backoff_ms
            .saturating_mul(2)
            .min(WORKER_RECONFIGURE_RETRY_MAX_MS)
    };
    (now_ms.saturating_add(backoff), backoff)
}

fn source_kind_from_uri(uri: &str) -> krusty::source::SourceKind {
    if uri.ends_with(".java") {
        return krusty::source::SourceKind::Java;
    }
    url::Url::parse(uri)
        .ok()
        .and_then(|uri| uri.to_file_path().ok())
        .as_deref()
        .and_then(krusty::source::kind)
        .unwrap_or(krusty::source::SourceKind::Kotlin)
}

fn project_module_assignments(
    snapshot: Option<&krusty_lsp::project::model::SourceModuleGraph>,
    documents: &[(&str, &str)],
) -> Vec<Option<usize>> {
    snapshot.map_or_else(
        || vec![Some(0); documents.len()],
        |snapshot| {
            let model = snapshot.model();
            if matches!(model.kind, ProviderKind::Explicit | ProviderKind::None) {
                return vec![Some(0); documents.len()];
            }
            documents
                .iter()
                .map(|(uri, _)| {
                    url::Url::parse(uri)
                        .ok()
                        .and_then(|uri| uri.to_file_path().ok())
                        .and_then(|path| snapshot.module_index_for_source(&path))
                })
                .collect()
        },
    )
}

fn project_analysis_groups(
    module_assignments: &[Option<usize>],
) -> Vec<(Option<usize>, Vec<usize>)> {
    let mut groups: Vec<(Option<usize>, Vec<usize>)> = Vec::new();
    for (document_index, module_index) in module_assignments.iter().copied().enumerate() {
        let Some(module_index) = module_index else {
            continue;
        };
        if let Some((_, document_indices)) = groups
            .iter_mut()
            .find(|(candidate, _)| *candidate == Some(module_index))
        {
            document_indices.push(document_index);
        } else {
            groups.push((Some(module_index), vec![document_index]));
        }
    }
    groups
}

fn project_group_compiler_config(
    model: Option<&ProjectModel>,
    module_index: Option<usize>,
    platform_classpath: &[PathBuf],
    options: &LspOptions,
) -> (Option<Vec<PathBuf>>, Vec<String>) {
    let Some((model, module)) = model
        .zip(module_index)
        .and_then(|(model, index)| model.modules.get(index).map(|module| (model, module)))
    else {
        return (None, options.language_arguments().to_vec());
    };

    let mut classpath = model.compile_classpath(module);
    for entry in platform_classpath {
        if !classpath.contains(entry) {
            classpath.push(entry.clone());
        }
    }
    let mut language_arguments = module.kotlinc_args.clone();
    language_arguments.extend_from_slice(options.language_arguments());
    (Some(classpath), language_arguments)
}

struct ProjectAnalysisGroup {
    module_index: Option<usize>,
    document_indices: Vec<usize>,
    support_documents: Vec<(String, String)>,
    inferred_support_count: usize,
    java_sources: Vec<String>,
    navigation_file_remaps: Vec<(u32, u32)>,
}

struct CachedProjectAnalysis {
    module_index: Option<usize>,
    fingerprint: u64,
    document_indices: Vec<usize>,
    analyses: Vec<DocumentAnalysis>,
    retained_bytes: usize,
}

fn open_documents_from_modules<'a>(
    visible_indices: &[usize],
    documents: &[(&'a str, &'a str)],
    module_assignments: &[Option<usize>],
) -> Vec<(usize, String, String)> {
    documents
        .iter()
        .zip(module_assignments)
        .enumerate()
        .filter_map(|(document_index, ((uri, source), assignment))| {
            if assignment.is_some_and(|index| visible_indices.contains(&index)) {
                Some((document_index, (*uri).to_string(), (*source).to_string()))
            } else {
                None
            }
        })
        .collect()
}

/// The group's worker slots in wire order, each paired with the URI it may be dumped under.
///
/// One traversal so the source inputs and the dump URIs cannot drift apart: a slot's URI is empty
/// exactly when that slot is not a primary document of this group, and a dump of an empty URI is
/// never offered. Two kinds of slot are deliberately blank:
///
/// - open documents belonging to another group, whose text this group blanks out anyway; and
/// - the support tail, which carries friend and dependency sources — including *open* files from
///   another module. Those are analyzed here under this group's classpath and language arguments,
///   not their own module's, so dumping one would render unresolved types and feature errors for a
///   file the editor shows as clean.
///
/// The URI is a borrowed `&str` rather than an owned `String` because this traversal is on the
/// analysis hot path; only the dev-mode dump path pays for copies.
fn project_group_slots<'a>(
    documents: &'a [(&'a str, &'a str)],
    group: &'a ProjectAnalysisGroup,
) -> impl Iterator<Item = (&'a str, krusty::source::SourceInput<'a>)> + 'a {
    documents
        .iter()
        .enumerate()
        .map(move |(index, (uri, source))| {
            let in_group = group.document_indices.contains(&index);
            (
                if in_group { *uri } else { "" },
                krusty::source::SourceInput::new(
                    source_kind_from_uri(uri),
                    if in_group { source } else { "" },
                ),
            )
        })
        .chain(group.support_documents.iter().map(|(uri, source)| {
            (
                "",
                krusty::source::SourceInput::new(source_kind_from_uri(uri), source),
            )
        }))
}

fn project_group_inputs<'a>(
    documents: &'a [(&'a str, &'a str)],
    group: &'a ProjectAnalysisGroup,
) -> Vec<krusty::source::SourceInput<'a>> {
    project_group_slots(documents, group)
        .map(|(_, input)| input)
        .collect()
}

/// Dump URIs parallel to `project_group_inputs`, blank wherever the slot is not dumpable.
fn project_group_uris(documents: &[(&str, &str)], group: &ProjectAnalysisGroup) -> Vec<String> {
    project_group_slots(documents, group)
        .map(|(uri, _)| uri.to_string())
        .collect()
}

fn register_canonical_support(
    documents: &[(&str, &str)],
    group_support: &[(String, String)],
    support_documents: &mut Vec<(String, String)>,
    support_indices: &mut HashMap<String, usize>,
    mut remaining_bytes: usize,
    mut remaining_entries: usize,
    next_discarded_file: &mut u32,
) -> (Vec<(u32, u32)>, usize) {
    let mut added_bytes = 0usize;
    let mut remaps = Vec::with_capacity(group_support.len());
    for (local_index, (uri, source)) in group_support.iter().enumerate() {
        let canonical =
            if let Some(index) = documents.iter().position(|(open_uri, _)| open_uri == uri) {
                index as u32
            } else if let Some(index) = support_indices.get(uri) {
                (documents.len() + index) as u32
            } else if remaining_entries > 0 && source.len() <= remaining_bytes {
                let index = support_documents.len();
                support_indices.insert(uri.clone(), index);
                support_documents.push((uri.clone(), source.clone()));
                remaining_bytes -= source.len();
                remaining_entries -= 1;
                added_bytes += source.len();
                (documents.len() + index) as u32
            } else {
                let discarded = *next_discarded_file;
                *next_discarded_file = next_discarded_file.saturating_sub(1);
                discarded
            };
        remaps.push(((documents.len() + local_index) as u32, canonical));
    }
    (remaps, added_bytes)
}

fn project_group_fingerprint(documents: &[(&str, &str)], group: &ProjectAnalysisGroup) -> u64 {
    let mut fingerprint = DefaultHasher::new();
    documents.len().hash(&mut fingerprint);
    group.module_index.hash(&mut fingerprint);
    group.document_indices.hash(&mut fingerprint);
    group.inferred_support_count.hash(&mut fingerprint);
    group.navigation_file_remaps.hash(&mut fingerprint);
    for (index, (uri, source)) in documents.iter().enumerate() {
        if group.document_indices.contains(&index) {
            uri.hash(&mut fingerprint);
            source.hash(&mut fingerprint);
        }
    }
    for (uri, source) in &group.support_documents {
        uri.hash(&mut fingerprint);
        source.hash(&mut fingerprint);
    }
    group.java_sources.hash(&mut fingerprint);
    fingerprint.finish()
}

fn retain_analysis_cache_budget(
    cache: &mut Vec<CachedProjectAnalysis>,
    incoming_bytes: usize,
    max_bytes: usize,
) {
    let mut retained = cache
        .iter()
        .map(|cached| cached.retained_bytes)
        .sum::<usize>();
    while !cache.is_empty() && incoming_bytes > max_bytes.saturating_sub(retained) {
        retained = retained.saturating_sub(cache.remove(0).retained_bytes);
    }
}

fn source_bytes<'a>(sources: impl IntoIterator<Item = &'a str>) -> Option<usize> {
    sources
        .into_iter()
        .try_fold(0usize, |bytes, source| bytes.checked_add(source.len()))
}

fn project_source_size_limit_message() -> String {
    format!(
        "module source set exceeds analysis limit (maximum {} MiB); semantic diagnostics suppressed",
        krusty_lsp::MAX_SOURCE_SET_BYTES / (1024 * 1024)
    )
}

fn project_source_error_analysis(message: &str) -> DocumentAnalysis {
    DocumentAnalysis::with_diagnostics(vec![krusty::diag::Diagnostic {
        span: krusty::diag::Span::new(0, 0),
        editor_span: None,
        identity: None,
        severity: krusty::diag::Severity::Error,
        kind: krusty::diag::DiagnosticKind::Compiler,
        msg: message.to_string(),
        file: 0,
    }])
}

fn fail_project_group(
    analyses: &mut [DocumentAnalysis],
    cache: &mut Vec<CachedProjectAnalysis>,
    module_index: Option<usize>,
    document_indices: &[usize],
    message: &str,
) {
    cache.retain(|cached| cached.module_index != module_index);
    for &index in document_indices {
        analyses[index] = project_source_error_analysis(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_command_rejects_missing_values_and_unknown_options() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            parse_cache_command(&args(&["clean", "--all", "-deps-cache-dir", "/cache"])).unwrap(),
            (true, Some(PathBuf::from("/cache")))
        );
        assert!(parse_cache_command(&args(&["clean", "-deps-cache-dir"])).is_err());
        assert!(parse_cache_command(&args(&["clean", "--unknown"])).is_err());
    }

    #[test]
    fn worker_parent_protocol_is_required_and_removed_before_option_parsing() {
        let mut arguments = vec![
            "-classpath".to_string(),
            "/project/classes".to_string(),
            "--analysis-worker".to_string(),
            "42".to_string(),
        ];
        assert_eq!(take_worker_parent(&mut arguments).unwrap(), Some(42));
        assert_eq!(arguments, ["-classpath", "/project/classes"]);

        let mut missing = vec!["--analysis-worker".to_string()];
        assert!(take_worker_parent(&mut missing).is_err());
        let mut invalid = vec!["--analysis-worker".to_string(), "parent".to_string()];
        assert!(take_worker_parent(&mut invalid).is_err());
    }

    #[test]
    fn project_logs_bound_the_classpath_listing() {
        let classpath = (0..61)
            .map(|index| PathBuf::from(format!("/classpath/{index}.jar")))
            .collect::<Vec<_>>();

        let logs = WorkerHost::describe_model(ProviderKind::Gradle, 4, &classpath, None);

        assert_eq!(
            logs[0],
            "krusty: gradle — 4 module(s), 61 classpath entries"
        );
        assert_eq!(logs[1], "krusty: JDK = none");
        assert!(logs[2].contains("/classpath/59.jar"));
        assert!(!logs[2].contains("/classpath/60.jar"));
        assert!(logs[2].ends_with("… 1 more"));
    }

    #[test]
    fn project_analysis_groups_preserve_global_slots() {
        let mut dependency = krusty_lsp::project::Module::new(
            krusty_lsp::project::ModuleId::new(":dependency", "main"),
            "/workspace/dependency",
        );
        dependency.source_roots = vec![krusty_lsp::project::SourceRoot::source(
            "/workspace/dependency/src",
        )];
        let mut consumer = krusty_lsp::project::Module::new(
            krusty_lsp::project::ModuleId::new(":consumer", "main"),
            "/workspace/consumer",
        );
        consumer.source_roots = vec![krusty_lsp::project::SourceRoot::source(
            "/workspace/consumer/src",
        )];
        consumer.depends_on = vec![krusty_lsp::project::ModuleId::new(":dependency", "main")];
        let model = krusty_lsp::ProjectModel::new("/workspace", krusty_lsp::ProviderKind::Gradle)
            .with_modules(vec![dependency, consumer]);
        let dependency_uri = url::Url::from_file_path("/workspace/dependency/src/First.kt")
            .unwrap()
            .to_string();
        let consumer_uri = url::Url::from_file_path("/workspace/consumer/src/Second.kt")
            .unwrap()
            .to_string();
        let unowned_uri = url::Url::from_file_path("/workspace/other/Third.kt")
            .unwrap()
            .to_string();
        let documents = [
            (dependency_uri.as_str(), "fun same() {}"),
            (unowned_uri.as_str(), "fun same() {}"),
            (consumer_uri.as_str(), "fun same() {}"),
        ];
        let module_graph = model.clone().into_source_module_graph();
        let assignments = project_module_assignments(Some(&module_graph), &documents);

        assert_eq!(assignments, [Some(0), None, Some(1)]);
        assert_eq!(
            project_analysis_groups(&assignments),
            [(Some(0), vec![0]), (Some(1), vec![2])]
        );
        assert!(project_analysis_groups(&[None, None]).is_empty());
        assert_eq!(
            open_documents_from_modules(
                &module_graph.get(1).unwrap().dependencies,
                &documents,
                &assignments,
            ),
            [(0, dependency_uri.clone(), documents[0].1.to_string())]
        );

        let dependency_support = (
            "file:///dependency-support.kt".into(),
            "fun helper() {}".into(),
        );
        let consumer_support = (
            "file:///consumer-support.kt".into(),
            "fun helper() {}".into(),
        );
        let dependency_group = ProjectAnalysisGroup {
            module_index: Some(0),
            document_indices: vec![0],
            support_documents: vec![dependency_support.clone()],
            inferred_support_count: 1,
            java_sources: Vec::new(),
            navigation_file_remaps: vec![(3, 3)],
        };
        let consumer_group = ProjectAnalysisGroup {
            module_index: Some(1),
            document_indices: vec![2],
            support_documents: vec![
                consumer_support.clone(),
                (dependency_uri.clone(), documents[0].1.into()),
            ],
            inferred_support_count: 1,
            java_sources: Vec::new(),
            navigation_file_remaps: vec![(3, 4), (4, 0)],
        };
        let dependency_inputs = project_group_inputs(&documents, &dependency_group);
        assert_eq!(
            dependency_inputs
                .iter()
                .map(|input| input.text)
                .collect::<Vec<_>>(),
            [documents[0].1, "", "", dependency_support.1.as_str()]
        );
        let consumer_inputs = project_group_inputs(&documents, &consumer_group);
        assert_eq!(
            consumer_inputs
                .iter()
                .map(|input| input.text)
                .collect::<Vec<_>>(),
            [
                "",
                "",
                documents[2].1,
                consumer_support.1.as_str(),
                documents[0].1
            ]
        );
        let fingerprint = project_group_fingerprint(&documents, &consumer_group);
        let unrelated_changed = [
            documents[0],
            (documents[1].0, "fun changed() {}"),
            documents[2],
        ];
        assert_eq!(
            project_group_fingerprint(&unrelated_changed, &consumer_group),
            fingerprint
        );
        let consumer_changed = [
            documents[0],
            documents[1],
            (documents[2].0, "fun changed() {}"),
        ];
        assert_ne!(
            project_group_fingerprint(&consumer_changed, &consumer_group),
            fingerprint
        );

        let mut failures = (0..documents.len())
            .map(|_| DocumentAnalysis::empty())
            .collect::<Vec<_>>();
        let mut cache = Vec::new();
        fail_project_group(&mut failures, &mut cache, Some(0), &[0], "failed");
        fail_project_group(&mut failures, &mut cache, Some(1), &[2], "failed");
        assert_eq!(failures[0].diagnostics[0].msg, "failed");
        assert!(failures[1].diagnostics.is_empty());
        assert_eq!(failures[2].diagnostics[0].msg, "failed");

        let fallback_snapshot =
            krusty_lsp::ProjectModel::new("/workspace", krusty_lsp::ProviderKind::None)
                .with_modules(model.modules)
                .into_source_module_graph();
        assert_eq!(
            project_module_assignments(Some(&fallback_snapshot), &documents),
            [Some(0), Some(0), Some(0)]
        );
        assert_eq!(
            project_module_assignments(None, &documents),
            [Some(0), Some(0), Some(0)]
        );
    }

    #[test]
    fn project_group_config_is_module_scoped() {
        let mut first = krusty_lsp::project::Module::new(
            krusty_lsp::project::ModuleId::new(":first", "main"),
            "/workspace/first",
        );
        first.classpath = vec![PathBuf::from("/deps/first.jar")];
        first.kotlinc_args = vec!["-XXLanguage:+NameBasedDestructuring".to_string()];
        let mut second = krusty_lsp::project::Module::new(
            krusty_lsp::project::ModuleId::new(":second", "main"),
            "/workspace/second",
        );
        second.classpath = vec![PathBuf::from("/deps/second.jar")];
        let model =
            ProjectModel::new("/workspace", ProviderKind::Gradle).with_modules(vec![first, second]);
        let options = LspOptions::parse(Vec::<String>::new()).unwrap();
        let platform = [PathBuf::from("/jdk/lib/modules")];

        let (classpath, language_arguments) =
            project_group_compiler_config(Some(&model), Some(0), &platform, &options);

        assert_eq!(
            classpath.unwrap(),
            [
                PathBuf::from("/deps/first.jar"),
                PathBuf::from("/jdk/lib/modules")
            ]
        );
        assert_eq!(language_arguments, ["-XXLanguage:+NameBasedDestructuring"]);
        let (classpath, language_arguments) =
            project_group_compiler_config(Some(&model), Some(1), &platform, &options);
        assert_eq!(
            classpath.unwrap(),
            [
                PathBuf::from("/deps/second.jar"),
                PathBuf::from("/jdk/lib/modules")
            ]
        );
        assert!(language_arguments.is_empty());
    }

    #[test]
    fn document_admission_is_bounded_per_modeled_module() {
        let mut first = krusty_lsp::project::Module::new(
            krusty_lsp::project::ModuleId::new(":first", "main"),
            "/workspace/first",
        );
        first.source_roots = vec![krusty_lsp::project::SourceRoot::source(
            "/workspace/first/src",
        )];
        let mut second = krusty_lsp::project::Module::new(
            krusty_lsp::project::ModuleId::new(":second", "main"),
            "/workspace/second",
        );
        second.source_roots = vec![krusty_lsp::project::SourceRoot::source(
            "/workspace/second/src",
        )];
        let mut consumer = krusty_lsp::project::Module::new(
            krusty_lsp::project::ModuleId::new(":consumer", "main"),
            "/workspace/consumer",
        );
        consumer.source_roots = vec![krusty_lsp::project::SourceRoot::source(
            "/workspace/consumer/src",
        )];
        consumer.depends_on = vec![krusty_lsp::project::ModuleId::new(":first", "main")];
        let model = krusty_lsp::ProjectModel::new("/workspace", ProviderKind::Gradle)
            .with_modules(vec![first, second, consumer]);
        let first_uri = "file:///workspace/first/src/First.kt";
        let first_other_uri = "file:///workspace/first/src/Other.kt";
        let second_uri = "file:///workspace/second/src/Second.kt";
        let consumer_uri = "file:///workspace/consumer/src/Consumer.kt";
        let large = krusty_lsp::MAX_SOURCE_SET_BYTES / 2 + 1;
        let snapshot = model.clone().into_source_module_graph();
        let admission = krusty_lsp::DocumentAdmission::for_snapshot(&snapshot);

        assert!(admission.accepts(&[(first_uri, large), (second_uri, large)]));
        assert!(admission.accepts(&[
            (first_uri, krusty_lsp::MAX_SOURCE_SET_BYTES),
            (second_uri, krusty_lsp::MAX_SOURCE_SET_BYTES),
        ]));
        assert!(!admission.accepts(&[
            (first_uri, krusty_lsp::MAX_SOURCE_SET_BYTES),
            (second_uri, krusty_lsp::MAX_SOURCE_SET_BYTES),
            ("file:///workspace/unowned/Extra.kt", 1),
        ]));
        assert!(!admission.accepts(&[(first_uri, large), (first_other_uri, large)]));
        assert!(!admission.accepts(&[(first_uri, large), (consumer_uri, large)]));
        assert!(!admission.accepts(&[(
            "file:///workspace/unowned/Oversized.kt",
            krusty_lsp::MAX_SOURCE_SET_BYTES + 1,
        )],));

        let fallback = krusty_lsp::ProjectModel::new("/workspace", ProviderKind::None)
            .with_modules(model.modules.clone());
        let fallback = fallback.into_source_module_graph();
        assert!(!krusty_lsp::DocumentAdmission::for_snapshot(&fallback)
            .accepts(&[(first_uri, large), (second_uri, large)]));
        assert!(!krusty_lsp::DocumentAdmission::default()
            .accepts(&[(first_uri, large), (second_uri, large)]));
    }

    #[test]
    fn module_analysis_cache_evicts_to_its_global_byte_budget() {
        let cached = |module_index, retained_bytes| CachedProjectAnalysis {
            module_index: Some(module_index),
            fingerprint: module_index as u64,
            document_indices: vec![module_index],
            analyses: vec![DocumentAnalysis::empty()],
            retained_bytes,
        };
        let mut cache = vec![cached(0, 4), cached(1, 4)];

        retain_analysis_cache_budget(&mut cache, 4, 8);

        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].module_index, Some(1));
    }

    #[test]
    fn two_consumers_share_one_canonical_support_target() {
        let documents = [
            ("file:///base-open.kt", "class Base"),
            ("file:///first-open.kt", "class First"),
            ("file:///second-open.kt", "class Second"),
        ];
        let shared = ("file:///shared.kt".to_string(), "class Shared".to_string());
        let first_support = vec![
            shared.clone(),
            (
                "file:///first-support.kt".into(),
                "class FirstSupport".into(),
            ),
            (documents[0].0.into(), documents[0].1.into()),
        ];
        let second_support = vec![
            shared.clone(),
            (
                "file:///second-support.kt".into(),
                "class SecondSupport".into(),
            ),
            (documents[0].0.into(), documents[0].1.into()),
        ];
        let mut support_documents = Vec::new();
        let mut support_indices = HashMap::new();
        let mut next_discarded = u32::MAX;

        let (first_remaps, _) = register_canonical_support(
            &documents,
            &first_support,
            &mut support_documents,
            &mut support_indices,
            usize::MAX,
            usize::MAX,
            &mut next_discarded,
        );
        let (second_remaps, _) = register_canonical_support(
            &documents,
            &second_support,
            &mut support_documents,
            &mut support_indices,
            usize::MAX,
            usize::MAX,
            &mut next_discarded,
        );

        assert_eq!(
            support_documents
                .iter()
                .filter(|(uri, _)| uri == &shared.0)
                .count(),
            1
        );
        assert_eq!(first_remaps[0].1, second_remaps[0].1);
        assert_eq!(first_remaps[2], (5, 0));
        assert_eq!(second_remaps[2], (5, 0));
    }

    #[test]
    fn canonical_support_budget_sheds_navigation_without_failing_analysis() {
        let documents = [("file:///open.kt", "class Open")];
        let support = vec![
            ("file:///kept.kt".to_string(), "x".to_string()),
            ("file:///shed.kt".to_string(), "yy".to_string()),
            ("file:///also-shed.kt".to_string(), "zzz".to_string()),
        ];
        let mut support_documents = Vec::new();
        let mut support_indices = HashMap::new();
        let mut next_discarded = u32::MAX;

        let (remaps, added_bytes) = register_canonical_support(
            &documents,
            &support,
            &mut support_documents,
            &mut support_indices,
            1,
            usize::MAX,
            &mut next_discarded,
        );

        assert_eq!(added_bytes, 1);
        assert_eq!(support_documents, [support[0].clone()]);
        assert_eq!(remaps, [(1, 1), (2, u32::MAX), (3, u32::MAX - 1)]);
        assert_eq!(next_discarded, u32::MAX - 2);
    }

    #[test]
    fn worker_reconfigure_retry_uses_capped_backoff() {
        assert_eq!(next_worker_reconfigure_retry(500, 0), (1_500, 1_000));
        assert_eq!(next_worker_reconfigure_retry(1_500, 1_000), (3_500, 2_000));
        assert_eq!(
            next_worker_reconfigure_retry(10_000, 30_000),
            (40_000, 30_000)
        );
        assert_eq!(
            next_worker_reconfigure_retry(u64::MAX - 10, 30_000),
            (u64::MAX, 30_000)
        );
    }

    #[test]
    fn interrupted_analysis_remains_pending() {
        assert!(analysis_remains_pending(io::ErrorKind::Interrupted));
        assert!(analysis_remains_pending(io::ErrorKind::TimedOut));
        assert!(!analysis_remains_pending(io::ErrorKind::UnexpectedEof));
    }

    #[test]
    fn recognizes_java_project_sources() {
        assert!(is_java_source_path(Path::new(
            "src/main/java/p/Widget.java"
        )));
        assert!(!is_java_source_path(Path::new(
            "src/main/kotlin/p/Widget.kt"
        )));
    }

    /// A two-module fixture whose consumer group carries a support tail holding the *open*
    /// dependency file, modelled on `project_analysis_groups_preserve_global_slots`.
    fn consumer_group_fixture() -> (String, String, ProjectAnalysisGroup) {
        let dependency_uri = url::Url::from_file_path("/workspace/dependency/src/First.kt")
            .unwrap()
            .to_string();
        let consumer_uri = url::Url::from_file_path("/workspace/consumer/src/Second.kt")
            .unwrap()
            .to_string();
        let group = ProjectAnalysisGroup {
            module_index: Some(1),
            document_indices: vec![2],
            support_documents: vec![
                (
                    "file:///consumer-support.kt".into(),
                    "fun helper() {}".into(),
                ),
                // Spliced in by `open_documents_from_modules`, so it carries the open text verbatim.
                (dependency_uri.clone(), "fun dependency() {}".into()),
            ],
            inferred_support_count: 1,
            java_sources: Vec::new(),
            navigation_file_remaps: vec![(3, 4), (4, 0)],
        };
        (dependency_uri, consumer_uri, group)
    }

    #[test]
    fn dump_uris_stay_parallel_to_the_group_inputs() {
        let (dependency_uri, consumer_uri, group) = consumer_group_fixture();
        let unowned_uri = url::Url::from_file_path("/workspace/other/Third.kt")
            .unwrap()
            .to_string();
        let documents = [
            (dependency_uri.as_str(), "fun dependency() {}"),
            (unowned_uri.as_str(), "fun unowned() {}"),
            (consumer_uri.as_str(), "fun consumer() {}"),
        ];

        let inputs = project_group_inputs(&documents, &group);
        let uris = project_group_uris(&documents, &group);

        assert_eq!(
            uris.len(),
            inputs.len(),
            "a dump index is an index into the worker's own slots"
        );
        // Every dumpable slot must hold the text of the URI it claims, whatever order the traversal
        // emits slots in.
        for (slot, uri) in uris.iter().enumerate() {
            if uri.is_empty() {
                continue;
            }
            let expected = documents
                .iter()
                .find(|(open_uri, _)| open_uri == uri)
                .unwrap_or_else(|| panic!("slot {slot} names an unknown document {uri}"))
                .1;
            assert_eq!(
                inputs[slot].text, expected,
                "slot {slot} does not hold the source of {uri}"
            );
        }
        assert_eq!(
            uris[2], consumer_uri,
            "the group's own document is dumpable"
        );
        assert_eq!(uris[0], "", "another group's document has no source here");
        assert_eq!(uris[1], "", "another group's document has no source here");
    }

    #[test]
    fn support_documents_are_not_dumpable_under_another_modules_configuration() {
        let (dependency_uri, consumer_uri, group) = consumer_group_fixture();
        let unowned_uri = url::Url::from_file_path("/workspace/other/Third.kt")
            .unwrap()
            .to_string();
        let documents = [
            (dependency_uri.as_str(), "fun dependency() {}"),
            (unowned_uri.as_str(), "fun unowned() {}"),
            (consumer_uri.as_str(), "fun consumer() {}"),
        ];
        let inputs = project_group_inputs(&documents, &group);
        let uris = project_group_uris(&documents, &group);
        let sources = inputs
            .iter()
            .map(|input| input.text.to_string())
            .collect::<Vec<_>>();
        let kinds = inputs.iter().map(|input| input.kind).collect::<Vec<_>>();

        // The dependency file is open and its real text sits in this group's support tail, but it is
        // analyzed here under the consumer module's classpath and language arguments.
        assert_eq!(sources[4], documents[0].1);
        assert_eq!(uris[3], "", "support slots are never dumpable");
        assert_eq!(uris[4], "", "support slots are never dumpable");

        let mut retained = RetainedAnalysis::default();
        retained.record(
            true,
            &AnalysisPayload {
                sources: &sources,
                source_kinds: &kinds,
                uris: &uris,
                result_count: documents.len(),
                inferred_count: documents.len() + group.inferred_support_count,
                java_sources: &[],
                language_arguments: &["-Xcontext-parameters".to_string()],
                classpath: None,
            },
        );

        assert!(
            retained.locate(&dependency_uri).is_none(),
            "a file that is only support for another module must not be dumped under this \
             module's configuration"
        );
        assert_eq!(
            retained.locate(&consumer_uri).map(|(_, slot)| slot),
            Some(2)
        );
    }

    /// Record one group's payload with a single dumpable slot, as one pass over one module would.
    fn record_module(retained: &mut RetainedAnalysis, dev: bool, uri: &str, source: &str) {
        retained.record(
            dev,
            &AnalysisPayload {
                sources: &[source.to_string()],
                source_kinds: &[SourceKind::Kotlin],
                uris: &[uri.to_string()],
                result_count: 1,
                inferred_count: 1,
                java_sources: &[],
                language_arguments: &[],
                classpath: None,
            },
        );
    }

    #[test]
    fn every_group_in_a_pass_stays_dumpable() {
        let mut retained = RetainedAnalysis::default();

        // One pass over a workspace with a file open from each of two modules. The traversal order
        // is deterministic, so a single retained payload would leave the same module undumpable
        // forever rather than intermittently.
        retained.begin_pass();
        record_module(&mut retained, true, "file:///dependency/First.kt", "a");
        record_module(&mut retained, true, "file:///consumer/Second.kt", "b");

        assert_eq!(
            retained
                .locate("file:///dependency/First.kt")
                .map(|(group, slot)| (group.sources.clone(), slot)),
            Some((vec!["a".to_string()], 0)),
            "the group analyzed first must stay dumpable once a later group is analyzed"
        );
        assert_eq!(
            retained
                .locate("file:///consumer/Second.kt")
                .map(|(_, slot)| slot),
            Some(0)
        );
    }

    #[test]
    fn retained_dump_groups_share_one_global_byte_budget() {
        let mut retained = RetainedAnalysis::default();
        let source = vec!["body".to_string()];
        let kinds = vec![SourceKind::Kotlin];
        let first_uri = vec!["file:///first/First.kt".to_string()];
        let second_uri = vec!["file:///second/Second.kt".to_string()];
        fn payload<'a>(
            source: &'a [String],
            kinds: &'a [SourceKind],
            uris: &'a [String],
        ) -> AnalysisPayload<'a> {
            AnalysisPayload {
                sources: source,
                source_kinds: kinds,
                uris,
                result_count: 1,
                inferred_count: 1,
                java_sources: &[],
                language_arguments: &[],
                classpath: None,
            }
        }
        let first_group = retained_payload_bytes(&payload(&source, &kinds, &first_uri));
        let one_group = first_group.max(retained_payload_bytes(&payload(
            &source,
            &kinds,
            &second_uri,
        )));

        assert!(retained.record_with_budget(
            true,
            &payload(&source, &kinds, &first_uri),
            one_group
        ));
        assert!(
            !retained.record_with_budget(true, &payload(&source, &kinds, &second_uri), one_group),
            "a second module must not multiply retained source sets past the pass-wide budget"
        );
        assert_eq!(retained.groups.len(), 1);
        assert_eq!(retained.retained_bytes, first_group);

        retained.begin_pass();
        assert_eq!(retained.retained_bytes, 0);
        assert!(
            retained.record_with_budget(true, &payload(&source, &kinds, &second_uri), one_group),
            "a superseding pass must receive a fresh budget"
        );
    }

    /// A pass that serves a group from the analysis cache still leaves that group's files dumpable.
    ///
    /// Structural rather than behavioural: reaching the cache-hit arm needs a live worker process
    /// and a resolved project model, so what is pinned here is the property that made the arm
    /// matter — retention happens for the group regardless of which arm produces its analyses.
    #[test]
    fn retention_covers_a_pass_that_is_served_from_the_analysis_cache() {
        let source = include_str!("main.rs");
        let pass = source
            .split_once("fn analyze_open_documents(")
            .expect("the analysis pass")
            .1;
        let begin = pass
            .find("self.retained.begin_pass()")
            .expect("the pass drops the previous pass's payloads");
        let record = pass
            .find("self.retained.record(")
            .expect("the pass retains each group it reaches");
        let lookup = pass
            .find("self.analysis_cache.iter().position(")
            .expect("the pass consults the analysis cache");

        assert!(begin < record, "the pass must be cleared before it records");
        assert!(
            record < lookup,
            "retaining inside the cache-miss arm leaves a cached group undumpable, so a pass \
             triggered by a project change would drop every module but the last"
        );
    }

    #[test]
    fn a_new_pass_supersedes_the_previous_one_wholesale() {
        let mut retained = RetainedAnalysis::default();
        retained.begin_pass();
        record_module(&mut retained, true, "file:///a.kt", "a");

        retained.begin_pass();
        record_module(&mut retained, true, "file:///b.kt", "b");
        record_module(&mut retained, true, "file:///c.kt", "c");

        assert!(retained.locate("file:///b.kt").is_some());
        assert!(retained.locate("file:///c.kt").is_some());
        assert!(
            retained.locate("file:///a.kt").is_none(),
            "the superseded pass must be dropped, not accumulated"
        );
        assert_eq!(retained.groups.len(), 2);
    }

    #[test]
    fn a_repeat_dump_on_an_unchanged_document_is_not_re_rendered() {
        let mut retained = RetainedAnalysis::default();
        retained.begin_pass();
        record_module(&mut retained, true, "file:///a.kt", "fun foo() {}");
        let (group, _) = retained.locate("file:///a.kt").expect("retained");

        let mut rendered = RenderedDumps::default();
        assert_eq!(
            rendered.lookup("file:///a.kt", group.fingerprint),
            None,
            "the first request has nothing to reuse"
        );
        rendered.record(
            "file:///a.kt",
            group.fingerprint,
            Path::new("/cache/dumps/a.kt.md"),
        );

        // A burst of code action requests — 20 rapid presses, or one per cursor settle — hits the
        // same payload, so only the first re-analyzes the module group and rewrites the document.
        for _ in 0..20 {
            assert_eq!(
                rendered.lookup("file:///a.kt", group.fingerprint),
                Some(Path::new("/cache/dumps/a.kt.md"))
            );
        }
    }

    #[test]
    fn a_re_analyzed_document_is_rendered_again() {
        let mut retained = RetainedAnalysis::default();
        retained.begin_pass();
        record_module(&mut retained, true, "file:///a.kt", "fun foo() {}");
        let before = retained
            .locate("file:///a.kt")
            .expect("retained")
            .0
            .fingerprint;

        // Length-preserving: the payload's byte count is unchanged, so only a content-sensitive key
        // can tell the two apart.
        retained.begin_pass();
        record_module(&mut retained, true, "file:///a.kt", "fun bar() {}");
        let after = retained
            .locate("file:///a.kt")
            .expect("retained")
            .0
            .fingerprint;

        assert_ne!(before, after);

        let mut rendered = RenderedDumps::default();
        rendered.record("file:///a.kt", before, Path::new("/cache/dumps/a.kt.md"));
        assert_eq!(
            rendered.lookup("file:///a.kt", after),
            None,
            "a document analyzed since the last render must be rendered again"
        );
    }

    #[test]
    fn rendered_dumps_stay_bounded() {
        let mut rendered = RenderedDumps::default();
        for index in 0..(MAX_RENDERED_DUMPS * 3) {
            rendered.record(
                &format!("file:///{index}.kt"),
                index as u64,
                Path::new("/cache/dumps/x.md"),
            );
        }

        assert_eq!(rendered.entries.len(), MAX_RENDERED_DUMPS);
        assert!(
            rendered
                .lookup(
                    &format!("file:///{}.kt", MAX_RENDERED_DUMPS * 3 - 1),
                    (MAX_RENDERED_DUMPS * 3 - 1) as u64
                )
                .is_some(),
            "the most recent render must survive the bound"
        );

        // Re-rendering one document does not accumulate: its superseded entry can never be reused.
        rendered.record("file:///0.kt", 1, Path::new("/cache/dumps/x.md"));
        rendered.record("file:///0.kt", 2, Path::new("/cache/dumps/x.md"));
        assert_eq!(
            rendered
                .entries
                .iter()
                .filter(|entry| entry.uri == "file:///0.kt")
                .count(),
            1
        );
    }

    #[test]
    fn nothing_is_retained_outside_dev_mode() {
        let mut retained = RetainedAnalysis::default();
        let sources = ["a".to_string()];
        let uris = ["file:///a.kt".to_string()];
        let classpath = vec![PathBuf::from("/modules/lib.jar")];

        retained.begin_pass();
        retained.record(
            false,
            &AnalysisPayload {
                sources: &sources,
                source_kinds: &[SourceKind::Kotlin],
                uris: &uris,
                result_count: 1,
                inferred_count: 1,
                java_sources: &["class Stub {}".to_string()],
                language_arguments: &["-Xcontext-parameters".to_string()],
                classpath: Some(&classpath),
            },
        );

        assert!(retained.locate("file:///a.kt").is_none());
        assert!(
            retained.groups.is_empty(),
            "a non-dev session must retain nothing at all"
        );
    }

    #[test]
    fn the_retained_payload_carries_the_module_configuration_to_the_dump() {
        let mut retained = RetainedAnalysis::default();
        let sources = ["fun box() = 1".to_string(), "class Helper {}".to_string()];
        let uris = [
            "file:///w/Main.kt".to_string(),
            "file:///w/Helper.java".to_string(),
        ];
        let source_kinds = [SourceKind::Kotlin, SourceKind::Java];
        let java_sources = ["package p; class Stub {}".to_string()];
        let language_arguments = ["-Xname-based-destructuring".to_string()];
        let classpath = vec![PathBuf::from("/modules/lib.jar")];

        retained.record(
            true,
            &AnalysisPayload {
                sources: &sources,
                source_kinds: &source_kinds,
                uris: &uris,
                result_count: 2,
                inferred_count: 3,
                java_sources: &java_sources,
                language_arguments: &language_arguments,
                classpath: Some(&classpath),
            },
        );

        let (group, slot) = retained
            .locate("file:///w/Main.kt")
            .expect("the analyzed target must be retained");
        let cache_root = Path::new("/cache");
        let cache_key = "file:///w/Main.kt";
        let target = group.dump_target(slot, "Main.kt", cache_key, cache_root);

        assert_eq!(target.target, 0);
        assert_eq!(target.sources, sources);
        assert_eq!(
            target.source_kinds, source_kinds,
            "a Java document fed to the Kotlin parser fills the dump with spurious diagnostics"
        );
        assert_eq!(target.result_count, 2);
        assert_eq!(target.inferred_count, 3);
        assert_eq!(
            target.java_sources, java_sources,
            "without the Java stub overlay, references into Java sources stop resolving"
        );
        assert_eq!(
            target.language_arguments,
            Some(&language_arguments[..]),
            "the module's language arguments must not fall back to session features"
        );
        assert_eq!(
            target.classpath,
            Some(&classpath[..]),
            "without the module classpath the dump falls back to the launch -cp"
        );
        assert_eq!(target.label, "Main.kt");
        assert_eq!(target.cache_key, cache_key);
        assert_eq!(target.cache_root, cache_root);
    }

    #[test]
    fn dump_labels_are_workspace_relative() {
        let root = Path::new("/workspace");
        let uri = url::Url::from_file_path("/workspace/src/Main.kt")
            .unwrap()
            .to_string();

        assert_eq!(
            workspace_relative_label(Some(root), &uri),
            Path::new("src/Main.kt").to_string_lossy()
        );

        let outside = url::Url::from_file_path("/elsewhere/Other.kt")
            .unwrap()
            .to_string();
        assert_eq!(workspace_relative_label(Some(root), &outside), "Other.kt");
        assert_eq!(workspace_relative_label(None, &outside), "Other.kt");
        assert_eq!(
            workspace_relative_label(Some(root), "untitled:Untitled-1"),
            "untitled:Untitled-1"
        );
        assert_eq!(
            workspace_relative_label(Some(root), "editor:/virtual/Scratch.kt?session=opaque"),
            "editor:Scratch.kt",
            "URI query data must not become presentation text in a persisted dump"
        );
        let long = format!("editor:/{}", "x".repeat(4_096));
        assert!(
            workspace_relative_label(Some(root), &long).len() <= 1_027,
            "an untrusted virtual URI must not create an unbounded heading"
        );
    }
}
