use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use krusty_lsp::{
    detect, resolve_jdk, AnalysisWorker, DocumentAnalysis, JdkRequest, LibraryRef,
    LoadedProjectSources, LspOptions, MaterializedDefinition, ProcessRunner, ProjectFeedback,
    ProjectMessageKind, ProjectSources, ProjectSync, ProviderKind, RefreshOutcome,
    SystemEnvironment,
};

const WORKER_RECONFIGURE_RETRY_INITIAL_MS: u64 = 1_000;
const WORKER_RECONFIGURE_RETRY_MAX_MS: u64 = 30_000;

fn is_java_source_path(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("java")
}

fn analysis_remains_pending(kind: io::ErrorKind) -> bool {
    matches!(kind, io::ErrorKind::TimedOut | io::ErrorKind::Interrupted)
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

fn main() {
    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("cache") {
        run_cache_command(&arguments[1..]);
        return;
    }
    let worker_mode = arguments
        .iter()
        .position(|argument| argument == "--analysis-worker")
        .map(|index| arguments.remove(index))
        .is_some();
    let options = LspOptions::parse(arguments.clone()).unwrap_or_else(|error| {
        eprintln!("krusty-lsp: {error}");
        std::process::exit(2);
    });
    if worker_mode {
        let stdin = io::stdin();
        let stdout = io::stdout();
        if let Err(error) = krusty_lsp::run_analysis_worker(
            &mut stdin.lock(),
            &mut stdout.lock(),
            options.effective_classpath(),
        ) {
            eprintln!("krusty-lsp worker: {error}");
            std::process::exit(1);
        }
        return;
    }

    let worker = AnalysisWorker::spawn(
        std::env::current_exe().expect("locate krusty-lsp executable"),
        arguments,
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

    let host = WorkerHost::new(worker, options);
    match krusty_lsp::run_stdio_connection_async(host) {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("krusty-lsp: {error}");
            std::process::exit(1);
        }
    }
}

struct WorkerHost {
    worker: AnalysisWorker,
    options: LspOptions,
    runner: ProcessRunner,
    sync: Option<ProjectSync>,
    clock: Instant,
    root: Option<PathBuf>,
    jdk_warning_shown: bool,
    project_sources: ProjectSources,
    analysis_pending: bool,
    worker_reconfigure_retry_at_ms: Option<u64>,
    worker_reconfigure_retry_backoff_ms: u64,
}

impl WorkerHost {
    fn new(mut worker: AnalysisWorker, options: LspOptions) -> Self {
        worker.set_language_features(options.language_features());
        Self {
            worker,
            options,
            runner: ProcessRunner,
            sync: None,
            clock: Instant::now(),
            root: None,
            jdk_warning_shown: false,
            project_sources: ProjectSources::default(),
            analysis_pending: false,
            worker_reconfigure_retry_at_ms: None,
            worker_reconfigure_retry_backoff_ms: 0,
        }
    }

    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    fn configure(&mut self) -> ProjectFeedback {
        let previous_model = self.sync.as_ref().and_then(ProjectSync::model).cloned();
        let Some(sync) = self.sync.as_mut() else {
            return ProjectFeedback::default();
        };
        match sync.refresh(&self.runner) {
            RefreshOutcome::Unchanged => ProjectFeedback::default(),
            RefreshOutcome::Updated => {
                self.project_sources.invalidate();
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
                    sync.rollback_model(previous_model);
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

    fn finish_analysis(
        &mut self,
        result: io::Result<Vec<DocumentAnalysis>>,
        document_count: usize,
    ) -> Vec<DocumentAnalysis> {
        match result {
            Ok(analysis) => {
                self.analysis_pending = false;
                analysis
            }
            Err(error) if analysis_remains_pending(error.kind()) => {
                self.analysis_pending = true;
                eprintln!("krusty-lsp: {error}; source analysis remains pending");
                Vec::new()
            }
            Err(error) => {
                self.analysis_pending = false;
                (0..document_count)
                    .map(|_| {
                        DocumentAnalysis::with_diagnostics(vec![krusty::diag::Diagnostic {
                            span: krusty::diag::Span::new(0, 0),
                            editor_span: None,
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
}

impl krusty_lsp::Analysis for WorkerHost {
    fn analysis_ready(&self) -> bool {
        self.sync.as_ref().and_then(ProjectSync::model).is_some()
    }

    fn analysis_pending(&self) -> bool {
        self.analysis_pending
    }

    fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
        let result = self.worker.analyze(sources);
        self.finish_analysis(result, sources.len())
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
        let cache_root = self
            .options
            .deps_cache_dir()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| {
                krusty_lsp::deps_cache::default_cache_root(&|key| std::env::var(key).ok())
            });
        let path = krusty_lsp::deps_cache::store(&cache_root, &reference.fqn, &text).ok()?;
        Some(MaterializedDefinition {
            path,
            text,
            lo: span.lo,
            hi: span.hi,
        })
    }

    fn analyze_open_documents(
        &mut self,
        documents: &[(&str, &str)],
        open_uris: &[&str],
    ) -> (Vec<DocumentAnalysis>, Vec<(String, String)>) {
        let (support_documents, inferred_support_count, java_sources) =
            match self.project_support_sources(documents, open_uris) {
                Ok((sources, inferred_count, java_docs)) => {
                    (sources.to_vec(), inferred_count, java_docs)
                }
                Err(message) => {
                    self.analysis_pending = false;
                    let analyses = documents
                        .iter()
                        .map(|_| {
                            DocumentAnalysis::with_diagnostics(vec![krusty::diag::Diagnostic {
                                span: krusty::diag::Span::new(0, 0),
                                editor_span: None,
                                severity: krusty::diag::Severity::Error,
                                kind: krusty::diag::DiagnosticKind::Compiler,
                                msg: message.clone(),
                                file: 0,
                            }])
                        })
                        .collect();
                    return (analyses, Vec::new());
                }
            };
        let mut inputs = documents
            .iter()
            .map(|(uri, source)| {
                krusty::source::SourceInput::new(source_kind_from_uri(uri), source)
            })
            .collect::<Vec<_>>();
        inputs.extend(support_documents.iter().map(|(uri, source)| {
            krusty::source::SourceInput::new(source_kind_from_uri(uri), source)
        }));
        let analyses = self.worker.analyze_inputs_prefix(
            &inputs,
            documents.len(),
            documents.len() + inferred_support_count,
            &java_sources,
        );
        let analyses = self.finish_analysis(analyses, documents.len());
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
                    .and_then(ProjectSync::model)
                    .and_then(|model| model.module_for_source(path))
                    .is_some()
        });
        if is_project_source {
            self.project_sources.invalidate();
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

impl WorkerHost {
    fn project_support_sources(
        &mut self,
        documents: &[(&str, &str)],
        open_uris: &[&str],
    ) -> Result<LoadedProjectSources<'_>, String> {
        let Some(model) = self.sync.as_ref().and_then(ProjectSync::model) else {
            return Ok((&[], 0, Vec::new()));
        };
        self.project_sources.load(
            model,
            documents,
            open_uris,
            krusty_lsp::MAX_SOURCE_SET_BYTES,
        )
    }
}

fn source_kind_from_uri(uri: &str) -> krusty::source::SourceKind {
    url::Url::parse(uri)
        .ok()
        .and_then(|uri| uri.to_file_path().ok())
        .as_deref()
        .and_then(krusty::source::kind)
        .unwrap_or(krusty::source::SourceKind::Kotlin)
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
}
