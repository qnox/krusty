use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use krusty_lsp::{
    detect, resolve_jdk, AnalysisWorker, DocumentAnalysis, JdkRequest, LspOptions, ProcessRunner,
    ProjectFeedback, ProjectMessageKind, ProjectSync, RefreshOutcome, SystemEnvironment,
};

fn main() {
    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
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

    let host = WorkerHost::new(worker, options);
    match krusty_lsp::run_stdio_connection_with(host) {
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
}

impl WorkerHost {
    fn new(worker: AnalysisWorker, options: LspOptions) -> Self {
        Self {
            worker,
            options,
            runner: ProcessRunner,
            sync: None,
            clock: Instant::now(),
            root: None,
            jdk_warning_shown: false,
        }
    }

    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    fn configure(&mut self) -> ProjectFeedback {
        let Some(sync) = self.sync.as_mut() else {
            return ProjectFeedback::default();
        };
        match sync.refresh(&self.runner) {
            RefreshOutcome::Unchanged => ProjectFeedback::default(),
            RefreshOutcome::Updated => {
                let (classpath, jdk_home) = Self::launch_from(sync, &self.options, &self.runner);
                if let Err(error) =
                    self.worker
                        .reconfigure(&classpath, jdk_home.as_deref(), self.options.no_jdk())
                {
                    return ProjectFeedback {
                        reanalyze: false,
                        message: Some((
                            ProjectMessageKind::Error,
                            format!("krusty: could not restart analysis worker: {error}"),
                        )),
                    };
                }
                ProjectFeedback {
                    reanalyze: true,
                    message: self.jdk_warning(jdk_home.is_some()),
                }
            }
            RefreshOutcome::Failed {
                error,
                model_retained,
            } => {
                let kind = if model_retained {
                    ProjectMessageKind::Warning
                } else {
                    ProjectMessageKind::Error
                };
                ProjectFeedback {
                    reanalyze: false,
                    message: Some((kind, format!("krusty: project sync failed: {error}"))),
                }
            }
        }
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

impl krusty_lsp::Analysis for WorkerHost {
    fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
        self.worker.analyze(sources).unwrap_or_else(|error| {
            sources
                .iter()
                .map(|_| {
                    DocumentAnalysis::with_diagnostics(vec![krusty::diag::Diagnostic {
                        span: krusty::diag::Span::new(0, 0),
                        severity: krusty::diag::Severity::Error,
                        msg: format!("analysis worker failed: {error}"),
                        file: 0,
                    }])
                })
                .collect()
        })
    }

    fn set_workspace_root(&mut self, root: Option<PathBuf>) {
        let root = root
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let provider = detect(&root, self.options.explicit_classpath());
        self.root = Some(root);
        self.sync = Some(ProjectSync::new(provider));
        let _ = self.configure();
    }

    fn watched_globs(&mut self) -> Vec<String> {
        self.sync
            .as_ref()
            .map(ProjectSync::watch_globs)
            .unwrap_or_default()
    }

    fn note_project_change(&mut self) {
        let now = self.now_ms();
        if let Some(sync) = self.sync.as_mut() {
            sync.note_change(now);
        }
    }

    fn project_refresh_due_in(&self) -> Option<Duration> {
        self.sync
            .as_ref()
            .and_then(|sync| sync.refresh_due_in(self.now_ms()))
            .map(Duration::from_millis)
    }

    fn refresh_project(&mut self) -> ProjectFeedback {
        let now = self.now_ms();
        let Some(sync) = self.sync.as_mut() else {
            return ProjectFeedback::default();
        };
        if !sync.take_due(now) {
            return ProjectFeedback::default();
        }
        if let Some(root) = &self.root {
            sync.update_provider(detect(root, self.options.explicit_classpath()));
        }
        self.configure()
    }
}
