use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use krusty::jvm::classpath::platform_jdk_modules;
use krusty_lsp::{
    detect, resolve_jdk, AnalysisWorker, DocumentAnalysis, JdkRequest, LibraryRef, LspOptions,
    MaterializedDefinition, ProcessRunner, ProjectFeedback, ProjectMessageKind, ProjectModel,
    ProjectSources, ProjectSync, ProviderKind, RefreshOutcome, SystemEnvironment,
};

const WORKER_RECONFIGURE_RETRY_INITIAL_MS: u64 = 1_000;
const WORKER_RECONFIGURE_RETRY_MAX_MS: u64 = 30_000;
const MAX_RETAINED_SUPPORT_DOCUMENTS: usize = 32 * 1024;

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
    /// Set when the source inventory hit its per-root limit, so the shortfall is reported rather
    /// than looking like a fully indexed workspace.
    truncated_inventory: bool,
    project_sources: ProjectSources,
    analysis_cache: Vec<CachedProjectAnalysis>,
    analysis_pending: bool,
    platform_classpath: Vec<PathBuf>,
    worker_reconfigure_retry_at_ms: Option<u64>,
    worker_reconfigure_retry_backoff_ms: u64,
}

impl WorkerHost {
    fn new(mut worker: AnalysisWorker, options: LspOptions) -> Self {
        worker.set_language_features(options.language_features());
        let platform_classpath = if options.no_jdk() {
            Vec::new()
        } else {
            platform_jdk_modules(options.jdk_home())
                .into_iter()
                .collect()
        };
        Self {
            worker,
            options,
            runner: ProcessRunner,
            sync: None,
            clock: Instant::now(),
            root: None,
            jdk_warning_shown: false,
            truncated_inventory: false,
            project_sources: ProjectSources::default(),
            analysis_cache: Vec::new(),
            analysis_pending: false,
            platform_classpath,
            worker_reconfigure_retry_at_ms: None,
            worker_reconfigure_retry_backoff_ms: 0,
        }
    }

    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
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
                self.analysis_cache.clear();
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
                self.platform_classpath = if self.options.no_jdk() {
                    Vec::new()
                } else {
                    platform_jdk_modules(jdk_home.as_deref())
                        .into_iter()
                        .collect()
                };
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
                let text = std::fs::read_to_string(path).ok()?;
                // The open-document path is byte-bounded; indexing has to be too, or a generated
                // multi-hundred-megabyte source would sit in memory twice per chunk.
                budget = budget.checked_sub(text.len())?;
                Some(((*uri).to_string(), text))
            })
            .collect();
        if readable.is_empty() {
            return krusty_lsp::IndexOutcome::default();
        }
        // Deliberately NOT analyze_open_documents: that treats its argument as the open set and
        // evicts the interactive analysis cache, so every chunk would turn the user's next
        // keystroke into a cold recompile -- the regression the priority queue exists to avoid.
        let texts: Vec<&str> = readable.iter().map(|(_, text)| text.as_str()).collect();
        let analyses = self.analyze(&texts);
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
        let Some(snapshot) = self.sync.as_ref().and_then(ProjectSync::snapshot) else {
            return Vec::new();
        };
        let model = snapshot.model();
        let open_modules: std::collections::HashSet<usize> = open_uris
            .iter()
            .filter_map(|uri| krusty_lsp::uri::file_uri_to_path(uri))
            .filter_map(|path| model.module_index_for_source(&path))
            .collect();
        if open_modules.is_empty() {
            return Vec::new();
        }
        let (sources, _truncated) = krusty_lsp::project::workspace_sources(model);
        sources
            .into_iter()
            .filter(|path| {
                model
                    .module_index_for_source(path)
                    .is_some_and(|module| open_modules.contains(&module))
            })
            .filter_map(|path| krusty_lsp::uri::path_to_file_uri(&path))
            .collect()
    }

    /// Candidates come from the project model's own source inventory. Walking the tree separately
    /// here would be a second, divergent definition of what counts as a workspace source.
    fn workspace_index_candidates(&mut self) -> Vec<String> {
        let Some(snapshot) = self.sync.as_ref().and_then(ProjectSync::snapshot) else {
            return Vec::new();
        };
        let (sources, truncated) = krusty_lsp::project::workspace_sources(snapshot.model());
        if truncated {
            // Silent truncation would look identical to a fully indexed workspace.
            self.truncated_inventory = true;
        }
        sources
            .into_iter()
            .filter_map(|path| krusty_lsp::uri::path_to_file_uri(&path))
            .collect()
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
        let module_assignments =
            project_module_assignments(self.sync.as_ref().and_then(ProjectSync::model), documents);
        let group_seeds = project_analysis_groups(&module_assignments);
        let module_relations = self.sync.as_ref().and_then(ProjectSync::snapshot);
        let mut analyses = (0..documents.len())
            .map(|_| DocumentAnalysis::empty())
            .collect::<Vec<_>>();
        let mut workspace_symbols = krusty_lsp::WorkspaceSymbolIndex::default();
        self.analysis_pending = false;

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
                let (classpath, language_arguments) = project_group_compiler_config(
                    self.sync.as_ref().and_then(ProjectSync::model),
                    group.module_index,
                    &self.platform_classpath,
                    &self.options,
                );
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
        self.analysis_cache.clear();
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
    model: Option<&ProjectModel>,
    documents: &[(&str, &str)],
) -> Vec<Option<usize>> {
    model.map_or_else(
        || vec![Some(0); documents.len()],
        |model| {
            if matches!(model.kind, ProviderKind::Explicit | ProviderKind::None) {
                return vec![Some(0); documents.len()];
            }
            documents
                .iter()
                .map(|(uri, _)| {
                    url::Url::parse(uri)
                        .ok()
                        .and_then(|uri| uri.to_file_path().ok())
                        .and_then(|path| model.module_index_for_source(&path))
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

fn project_group_inputs<'a>(
    documents: &[(&'a str, &'a str)],
    group: &'a ProjectAnalysisGroup,
) -> Vec<krusty::source::SourceInput<'a>> {
    documents
        .iter()
        .enumerate()
        .map(|(index, (uri, source))| {
            krusty::source::SourceInput::new(
                source_kind_from_uri(uri),
                if group.document_indices.contains(&index) {
                    source
                } else {
                    ""
                },
            )
        })
        .chain(group.support_documents.iter().map(|(uri, source)| {
            krusty::source::SourceInput::new(source_kind_from_uri(uri), source)
        }))
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
        let assignments = project_module_assignments(Some(&model), &documents);
        let module_graph = model.clone().into_source_module_graph();

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

        let fallback_model =
            krusty_lsp::ProjectModel::new("/workspace", krusty_lsp::ProviderKind::None)
                .with_modules(model.modules);
        assert_eq!(
            project_module_assignments(Some(&fallback_model), &documents),
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
}
