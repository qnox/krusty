use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use krusty_lsp::{
    detect, resolve_jdk, AnalysisWorker, DocumentAnalysis, JdkRequest, LspOptions, ProcessRunner,
    ProjectFeedback, ProjectMessageKind, ProjectSync, ProviderKind, RefreshOutcome,
    SystemEnvironment,
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
                    return ProjectFeedback {
                        reanalyze: false,
                        message: Some((
                            ProjectMessageKind::Error,
                            format!("krusty: could not restart analysis worker: {error}"),
                        )),
                        logs,
                    };
                }
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

impl krusty_lsp::Analysis for WorkerHost {
    fn analysis_ready(&self) -> bool {
        self.sync.as_ref().and_then(ProjectSync::model).is_some()
    }

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
