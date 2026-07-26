use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use krusty_lsp::{
    detect, resolve_jdk, AnalysisWorker, DocumentAnalysis, JdkRequest, LspOptions, ProcessRunner,
    ProjectFeedback, ProjectMessageKind, ProjectSync, ProviderKind, RefreshOutcome,
    SystemEnvironment,
};

const MAX_SUPPORT_INVENTORY_ENTRIES: usize = 32 * 1024;

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
    support_cache: Option<SupportSourceCache>,
}

struct SupportSourceCache {
    roots: Vec<PathBuf>,
    excluded_paths: Vec<PathBuf>,
    documents: Vec<(String, String)>,
    bytes: usize,
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
            support_cache: None,
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
                self.support_cache = None;
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

    fn analyze_open_documents(
        &mut self,
        documents: &[(&str, &str)],
    ) -> (Vec<DocumentAnalysis>, Vec<(String, String)>) {
        let support_documents = match self.project_support_sources(documents) {
            Ok(sources) => sources.to_vec(),
            Err(message) => {
                let analyses = documents
                    .iter()
                    .map(|_| {
                        DocumentAnalysis::with_diagnostics(vec![krusty::diag::Diagnostic {
                            span: krusty::diag::Span::new(0, 0),
                            severity: krusty::diag::Severity::Error,
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
        let analyses = self
            .worker
            .analyze_inputs_prefix(&inputs, documents.len())
            .unwrap_or_else(|error| {
                documents
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
            });
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
        for extension in krusty::source::SUPPORTED_EXTENSIONS {
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
            is_project_configuration(path)
                || self
                    .sync
                    .as_ref()
                    .is_some_and(|sync| sync.watch_paths().iter().any(|watched| watched == path))
        });
        if is_project_change {
            self.note_project_change();
            return false;
        }
        let is_kotlin_source = path.as_ref().is_some_and(|path| {
            krusty::source::is_supported_path(path)
                && self
                    .sync
                    .as_ref()
                    .and_then(ProjectSync::model)
                    .is_some_and(|model| {
                        model.modules.iter().any(|module| {
                            module
                                .source_roots
                                .iter()
                                .any(|root| path.starts_with(&root.path))
                        })
                    })
        });
        if is_kotlin_source {
            self.support_cache = None;
            true
        } else {
            self.note_project_change();
            false
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

impl WorkerHost {
    fn project_support_sources(
        &mut self,
        documents: &[(&str, &str)],
    ) -> Result<&[(String, String)], String> {
        let Some(model) = self.sync.as_ref().and_then(ProjectSync::model) else {
            return Ok(&[]);
        };
        let open_paths = documents
            .iter()
            .filter_map(|(uri, _)| url::Url::parse(uri).ok()?.to_file_path().ok())
            .collect::<HashSet<_>>();
        let open_bytes = documents
            .iter()
            .map(|(_, source)| source.len())
            .sum::<usize>();
        let remaining = krusty_lsp::MAX_SOURCE_SET_BYTES.saturating_sub(open_bytes);
        let mut roots = Vec::new();
        for open_path in &open_paths {
            let matching_module = model
                .modules
                .iter()
                .filter(|module| {
                    module
                        .source_roots
                        .iter()
                        .any(|root| open_path.starts_with(&root.path))
                })
                .max_by_key(|module| {
                    module
                        .source_roots
                        .iter()
                        .filter(|root| open_path.starts_with(&root.path))
                        .map(|root| root.path.components().count())
                        .max()
                        .unwrap_or_default()
                });
            if let Some(module) = matching_module {
                for root in &module.source_roots {
                    if !roots.contains(&root.path) {
                        roots.push(root.path.clone());
                    }
                }
            }
        }

        let open_directories = open_paths
            .iter()
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .collect::<HashSet<_>>();
        roots.sort();
        roots.dedup();
        let mut excluded_paths = open_paths.iter().cloned().collect::<Vec<_>>();
        excluded_paths.sort();
        let cache_matches = self
            .support_cache
            .as_ref()
            .is_some_and(|cache| cache.roots == roots && cache.excluded_paths == excluded_paths);
        if cache_matches {
            let cache = self.support_cache.as_ref().unwrap();
            if cache.bytes > remaining {
                return Err(source_set_limit_message());
            }
            return Ok(&cache.documents);
        }

        let mut inventory_entries = MAX_SUPPORT_INVENTORY_ENTRIES;
        let mut paths = Vec::new();
        for root in &roots {
            paths.extend(kotlin_sources(root, &mut inventory_entries)?);
        }
        paths.retain(|path| !open_paths.contains(path));
        paths.sort_by_key(|path| {
            (
                !path
                    .parent()
                    .is_some_and(|parent| open_directories.contains(parent)),
                path.clone(),
            )
        });
        paths.dedup();

        let budget = krusty_lsp::MAX_SOURCE_SET_BYTES.saturating_sub(open_bytes);
        let (support_documents, bytes) = load_support_documents(paths, remaining, budget)?;
        self.support_cache = Some(SupportSourceCache {
            roots,
            excluded_paths,
            documents: support_documents,
            bytes,
        });
        Ok(&self.support_cache.as_ref().unwrap().documents)
    }
}

fn load_support_documents(
    paths: Vec<PathBuf>,
    mut remaining: usize,
    budget: usize,
) -> Result<(Vec<(String, String)>, usize), String> {
    let mut inventory = Vec::new();
    for path in paths {
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        let Ok(bytes) = usize::try_from(metadata.len()) else {
            return Err(source_set_limit_message());
        };
        if bytes > remaining {
            return Err(source_set_limit_message());
        }
        remaining -= bytes;
        inventory.push(path);
    }

    let mut bytes = 0usize;
    let mut documents = Vec::with_capacity(inventory.len());
    for path in inventory {
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(next_bytes) = bytes.checked_add(source.len()) else {
            return Err(source_set_limit_message());
        };
        if next_bytes > budget {
            return Err(source_set_limit_message());
        }
        bytes = next_bytes;
        let Ok(uri) = url::Url::from_file_path(path) else {
            continue;
        };
        documents.push((uri.into(), source));
    }
    Ok((documents, bytes))
}

fn source_set_limit_message() -> String {
    format!(
        "module source set exceeds analysis limit (maximum {} MiB); semantic diagnostics suppressed",
        krusty_lsp::MAX_SOURCE_SET_BYTES / (1024 * 1024)
    )
}

fn source_kind_from_uri(uri: &str) -> krusty::source::SourceKind {
    url::Url::parse(uri)
        .ok()
        .and_then(|uri| uri.to_file_path().ok())
        .as_deref()
        .and_then(krusty::source::kind)
        .unwrap_or(krusty::source::SourceKind::Kotlin)
}

fn is_project_configuration(path: &Path) -> bool {
    let file_name = path.file_name().and_then(|name| name.to_str());
    matches!(
        file_name,
        Some(
            "build.gradle"
                | "build.gradle.kts"
                | "settings.gradle"
                | "settings.gradle.kts"
                | "gradle.properties"
                | "libs.versions.toml"
                | "gradle-wrapper.properties"
                | "gradle.lockfile"
                | "pom.xml"
                | "BUILD"
                | "BUILD.bazel"
                | "build.sbt"
                | "build.sc"
        )
    ) || path.extension().is_some_and(|extension| extension == "bzl")
        || (path
            .extension()
            .is_some_and(|extension| extension == "json")
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|directory| directory == ".bsp"))
}

fn kotlin_sources(root: &Path, remaining_entries: &mut usize) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(next_remaining) = remaining_entries.checked_sub(1) else {
                return Err(source_set_limit_message());
            };
            *remaining_entries = next_remaining;
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && krusty::source::is_supported_path(&path) {
                sources.push(path);
            }
        }
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    fn oversized_support_sources_are_rejected_before_they_are_read() {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "krusty-lsp-oversized-support-{}-{unique}.kt",
            std::process::id()
        ));
        let file = fs::File::create(&path).unwrap();
        file.set_len(krusty_lsp::MAX_SOURCE_SET_BYTES as u64 + 1)
            .unwrap();

        let result = load_support_documents(
            vec![path.clone()],
            krusty_lsp::MAX_SOURCE_SET_BYTES,
            krusty_lsp::MAX_SOURCE_SET_BYTES,
        );

        fs::remove_file(path).ok();
        assert_eq!(result.unwrap_err(), source_set_limit_message());
    }

    #[test]
    fn support_inventory_is_entry_bounded() {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "krusty-lsp-support-inventory-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("First.kt"), "").unwrap();
        fs::write(directory.join("Second.kt"), "").unwrap();
        let mut remaining_entries = 1;

        let result = kotlin_sources(&directory, &mut remaining_entries);

        fs::remove_dir_all(directory).ok();
        assert_eq!(result.unwrap_err(), source_set_limit_message());
    }

    #[test]
    fn kotlin_gradle_scripts_are_project_configuration_not_support_sources() {
        assert!(is_project_configuration(Path::new(
            "/workspace/build.gradle.kts"
        )));
        assert!(!is_project_configuration(Path::new(
            "/workspace/src/Feature.kt"
        )));
    }
}
