//! Content-guarded project-model refresh with last-good retention.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use krusty::features::LangFeatures;

use super::fingerprint::{fingerprint_files, Fingerprint};
use super::model::{CanonicalPathCache, ProjectModel, ProviderKind, SourceModuleGraph};
use super::provider::{ProbeError, ProjectProvider};
use super::runner::CommandRunner;

const DEBOUNCE_MS: u64 = 750;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    Unchanged,
    Updated,
    Failed {
        error: ProbeError,
        model_retained: bool,
    },
}

pub struct ProjectSync {
    provider: Box<dyn ProjectProvider>,
    snapshot: Option<SourceModuleGraph>,
    fingerprint: Option<Fingerprint>,
    dirty_since: Option<u64>,
}

impl ProjectSync {
    pub fn new(provider: Box<dyn ProjectProvider>) -> Self {
        Self {
            provider,
            snapshot: None,
            fingerprint: None,
            dirty_since: None,
        }
    }

    pub fn kind(&self) -> ProviderKind {
        self.provider.kind()
    }

    pub fn model(&self) -> Option<&ProjectModel> {
        self.snapshot.as_ref().map(SourceModuleGraph::model)
    }

    pub fn snapshot(&self) -> Option<&SourceModuleGraph> {
        self.snapshot.as_ref()
    }

    pub fn rollback_snapshot(&mut self, snapshot: Option<SourceModuleGraph>) {
        self.snapshot = snapshot;
        self.fingerprint = None;
    }

    pub fn watch_paths(&self) -> Vec<PathBuf> {
        self.provider.watch_paths()
    }

    pub fn update_provider(&mut self, provider: Box<dyn ProjectProvider>) {
        if self.provider.kind() != provider.kind() || self.provider.root() != provider.root() {
            self.fingerprint = None;
        }
        self.provider = provider;
    }

    pub fn note_change(&mut self, now_ms: u64) {
        self.dirty_since = Some(now_ms);
    }

    pub fn refresh_due_in(&self, now_ms: u64) -> Option<u64> {
        self.dirty_since
            .map(|since| DEBOUNCE_MS.saturating_sub(now_ms.saturating_sub(since)))
    }

    pub fn take_due(&mut self, now_ms: u64) -> bool {
        if self.refresh_due_in(now_ms) != Some(0) {
            return false;
        }
        self.dirty_since = None;
        true
    }

    pub fn refresh(&mut self, runner: &dyn CommandRunner) -> RefreshOutcome {
        let fingerprint = fingerprint_files(
            &self.provider.watch_paths(),
            &self.provider.fingerprint_salt(),
        );
        if self.snapshot.is_some() && self.fingerprint == Some(fingerprint) {
            return RefreshOutcome::Unchanged;
        }
        let _lock = match super::lock::WorkspaceProbeLock::acquire(self.provider.root()) {
            Ok(lock) => lock,
            Err(error) => {
                return RefreshOutcome::Failed {
                    error: ProbeError::Io(format!(
                        "could not lock {}: {error}",
                        self.provider.root().display()
                    )),
                    model_retained: self.snapshot.is_some(),
                };
            }
        };
        match self.provider.probe(runner) {
            Ok(model) => {
                self.fingerprint = Some(fingerprint);
                self.snapshot = Some(model.into_source_module_graph());
                RefreshOutcome::Updated
            }
            Err(error) => RefreshOutcome::Failed {
                error,
                model_retained: self.snapshot.is_some(),
            },
        }
    }

    /// The union of every module's compile classpath.
    ///
    /// Used for worker startup and dependency-source materialization. Module analysis requests pass
    /// their narrower compile classpath. Project outputs precede published copies.
    pub fn project_classpath(&self) -> Vec<PathBuf> {
        let (mut outputs, dependencies) =
            self.project_classpath_parts_with(&|path| std::fs::canonicalize(path).ok());
        outputs.extend(dependencies);
        outputs
    }

    /// The classpath minus the project's own compiled output.
    ///
    /// What the project depends on, as opposed to what it produces. Indexing its own output as a
    /// dependency would list every workspace class twice -- once from its source and once as a stub
    /// decompiled from the class file beside it.
    pub fn dependency_classpath(&self) -> Vec<PathBuf> {
        self.project_classpath_parts_with(&|path| std::fs::canonicalize(path).ok())
            .1
    }

    #[cfg(test)]
    fn project_classpath_with(
        &self,
        canonicalize: &dyn Fn(&Path) -> Option<PathBuf>,
    ) -> Vec<PathBuf> {
        let (mut outputs, dependencies) = self.project_classpath_parts_with(canonicalize);
        outputs.extend(dependencies);
        outputs
    }

    /// Partition the deduplicated compile classpath once, using the same declared/canonical output
    /// identity for both consumers. Computing a leading-output count separately repeated the
    /// classification algorithm and made `dependency_classpath` depend on that copy staying in
    /// lockstep with the ordering copy.
    fn project_classpath_parts_with(
        &self,
        canonicalize: &dyn Fn(&Path) -> Option<PathBuf>,
    ) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let Some(model) = self.model() else {
            return (Vec::new(), Vec::new());
        };
        let mut union = Vec::new();
        let mut seen = HashSet::new();
        for module in &model.modules {
            for entry in model.compile_classpath(module) {
                if seen.insert(entry.clone()) {
                    union.push(entry);
                }
            }
        }
        let declared_output_set = model
            .modules
            .iter()
            .flat_map(|module| {
                module
                    .outputs
                    .iter()
                    .map(|output| output.path().to_path_buf())
                    .chain(module.friend_paths.iter().cloned())
            })
            .collect::<HashSet<_>>();
        let mut canonical_paths = CanonicalPathCache::new(canonicalize);
        let mut canonical_output_set = HashSet::new();
        for output in &declared_output_set {
            if let Some(canonical) = canonical_paths.get(output) {
                canonical_output_set.insert(canonical.to_path_buf());
            }
        }
        let mut is_output = |entry: &Path| {
            entry
                .ancestors()
                .any(|ancestor| declared_output_set.contains(ancestor))
                || canonical_paths.get(entry).is_some_and(|entry| {
                    entry
                        .ancestors()
                        .any(|ancestor| canonical_output_set.contains(ancestor))
                })
        };
        let (outputs, dependencies): (Vec<_>, Vec<_>) =
            union.into_iter().partition(|entry| is_output(entry));
        (outputs, dependencies)
    }

    /// The `jvmTarget` the project reports, used to pick a matching JDK.
    pub fn jvm_target(&self) -> Option<&str> {
        self.model()?
            .modules
            .iter()
            .find_map(|module| module.jvm_target.as_deref())
    }

    /// The union of language features enabled by any module.
    ///
    /// Default worker features for analyses without a modeled module.
    pub fn project_language_features(&self) -> LangFeatures {
        let mut project_features = LangFeatures::new();
        let Some(model) = self.model() else {
            return project_features;
        };
        for module in &model.modules {
            let mut module_features = LangFeatures::new();
            for argument in &module.kotlinc_args {
                module_features.apply_cli_arg(argument);
            }
            project_features.extend(&module_features);
        }
        project_features
    }

    /// Glob patterns to register with the editor's file watcher.
    pub fn watch_globs(&self) -> Vec<String> {
        self.provider.watch_globs()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::Path;

    use super::*;
    use crate::project::model::{Module, ModuleId, ModuleOutput, SourceRoot};
    use crate::project::runner::testing::FakeRunner;
    use crate::project::testing::TempTree;
    use crate::project::ProjectSources;

    /// A provider whose probe result the test controls, counting how often it ran.
    struct ScriptedProvider {
        kind: ProviderKind,
        watched: Vec<PathBuf>,
        results: RefCell<Vec<Result<ProjectModel, ProbeError>>>,
        probes: RefCell<usize>,
    }

    impl ScriptedProvider {
        fn new(watched: Vec<PathBuf>, results: Vec<Result<ProjectModel, ProbeError>>) -> Self {
            Self {
                kind: ProviderKind::Gradle,
                watched,
                results: RefCell::new(results),
                probes: RefCell::new(0),
            }
        }

        fn with_kind(mut self, kind: ProviderKind) -> Self {
            self.kind = kind;
            self
        }
    }

    impl ProjectProvider for ScriptedProvider {
        fn kind(&self) -> ProviderKind {
            self.kind
        }

        fn root(&self) -> &Path {
            Path::new("/p")
        }

        fn watch_paths(&self) -> Vec<PathBuf> {
            self.watched.clone()
        }

        fn probe(&self, _runner: &dyn CommandRunner) -> Result<ProjectModel, ProbeError> {
            *self.probes.borrow_mut() += 1;
            let mut results = self.results.borrow_mut();
            if results.is_empty() {
                return Err(ProbeError::Parse("no scripted result".to_string()));
            }
            results.remove(0)
        }
    }

    fn model_with(classpath: &str) -> ProjectModel {
        let mut module = Module::new(ModuleId::new(":app", "main"), "/p/app");
        module.source_roots = vec![SourceRoot::source("/p/app/src/main/kotlin")];
        module.classpath = vec![PathBuf::from(classpath)];
        ProjectModel::new("/p", ProviderKind::Gradle).with_modules(vec![module])
    }

    fn synced_project_classpath(model: ProjectModel) -> Vec<PathBuf> {
        let mut sync =
            ProjectSync::new(Box::new(ScriptedProvider::new(Vec::new(), vec![Ok(model)])));
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );
        sync.project_classpath()
    }

    fn synced_dependency_classpath(model: ProjectModel) -> Vec<PathBuf> {
        let mut sync =
            ProjectSync::new(Box::new(ScriptedProvider::new(Vec::new(), vec![Ok(model)])));
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );
        sync.dependency_classpath()
    }

    #[test]
    fn an_unchanged_fingerprint_does_not_run_the_build_tool_again() {
        let tree = TempTree::new("sync-unchanged");
        tree.write("build.gradle.kts", "dependencies {}");
        let provider = ScriptedProvider::new(
            vec![tree.path("build.gradle.kts")],
            vec![Ok(model_with("/m2/a.jar"))],
        );
        let mut sync = ProjectSync::new(Box::new(provider));

        assert!(matches!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        ));
        // Rewriting the same bytes is the common formatter/branch-switch case.
        tree.write("build.gradle.kts", "dependencies {}");
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Unchanged
        );
    }

    #[test]
    fn changed_content_reprobes_and_replaces_the_classpath() {
        let tree = TempTree::new("sync-changed");
        tree.write("build.gradle.kts", "dependencies {}");
        let mut sync = ProjectSync::new(Box::new(ScriptedProvider::new(
            vec![tree.path("build.gradle.kts")],
            vec![Ok(model_with("/m2/a.jar")), Ok(model_with("/m2/b.jar"))],
        )));

        sync.refresh(&FakeRunner::default());
        tree.write("build.gradle.kts", "dependencies { implementation(b) }");
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );
        assert_eq!(sync.project_classpath(), vec![PathBuf::from("/m2/b.jar")]);
    }

    #[test]
    fn project_language_features_union_recognized_module_arguments() {
        let mut first = Module::new(ModuleId::new(":first", "main"), "/p/first");
        first.kotlinc_args = vec![
            "-Xname-based-destructuring=complete".to_string(),
            "-Xunmodeled-option".to_string(),
        ];
        let mut second = Module::new(ModuleId::new(":second", "main"), "/p/second");
        second.kotlinc_args = vec!["-XXLanguage:+AnotherFeature".to_string()];
        let model = ProjectModel::new("/p", ProviderKind::Gradle).with_modules(vec![first, second]);
        let mut sync =
            ProjectSync::new(Box::new(ScriptedProvider::new(Vec::new(), vec![Ok(model)])));

        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );
        let features = sync.project_language_features();
        assert!(features.has("NameBasedDestructuring"));
        assert!(features.has("AnotherFeature"));
    }

    #[test]
    fn project_classpath_prefers_declared_gradle_outputs_even_outside_the_root() {
        let mut first = Module::new(ModuleId::new(":first", "main"), "/workspace/first");
        first.classpath = vec![
            PathBuf::from("/cache/published.jar"),
            PathBuf::from("/workspace/lib/checked-in.jar"),
            PathBuf::from("/workspace/app/build/generated/resources.jar"),
            PathBuf::from("/cache/support.jar"),
        ];
        let mut second = Module::new(ModuleId::new(":second", "main"), "/workspace/second");
        second.outputs = vec![ModuleOutput::classes("/composite/second/build/classes")];
        second.classpath = vec![
            PathBuf::from("/composite/second/build/classes"),
            PathBuf::from("/cache/published.jar"),
        ];
        let model =
            ProjectModel::new("/workspace", ProviderKind::Gradle).with_modules(vec![first, second]);

        assert_eq!(
            synced_project_classpath(model),
            vec![
                PathBuf::from("/composite/second/build/classes"),
                PathBuf::from("/cache/published.jar"),
                PathBuf::from("/workspace/lib/checked-in.jar"),
                PathBuf::from("/workspace/app/build/generated/resources.jar"),
                PathBuf::from("/cache/support.jar"),
            ]
        );
    }

    #[test]
    fn dependency_classpath_excludes_only_declared_project_outputs() {
        let mut library = Module::new(ModuleId::new(":library", "main"), "/workspace/library");
        library.outputs = vec![ModuleOutput::classes("/composite/library/classes")];
        let mut application = Module::new(
            ModuleId::new(":application", "main"),
            "/workspace/application",
        );
        application.classpath = vec![
            PathBuf::from("/cache/published.jar"),
            PathBuf::from("/workspace/checked-in.jar"),
            PathBuf::from("/composite/library/classes"),
        ];
        let model = ProjectModel::new("/workspace", ProviderKind::Gradle)
            .with_modules(vec![application, library]);

        // Being under the workspace root does not make an entry project output. Classification is
        // driven by the model's declared outputs, through the same partition used to put those
        // outputs first for worker startup.
        assert_eq!(
            synced_dependency_classpath(model),
            vec![
                PathBuf::from("/cache/published.jar"),
                PathBuf::from("/workspace/checked-in.jar"),
            ]
        );
    }

    #[test]
    fn project_classpath_prefers_declared_bsp_outputs() {
        let mut dependency = Module::new(ModuleId::new(":dependency", "main"), "/workspace");
        dependency.outputs = vec![ModuleOutput::location("/generated")];
        let mut consumer = Module::new(ModuleId::new(":consumer", "main"), "/workspace");
        consumer.classpath = vec![
            PathBuf::from("/cache/published.jar"),
            PathBuf::from("/generated/classes"),
        ];
        let model = ProjectModel::new("/workspace", ProviderKind::Bsp)
            .with_modules(vec![consumer, dependency]);

        assert_eq!(
            synced_project_classpath(model),
            vec![
                PathBuf::from("/generated/classes"),
                PathBuf::from("/cache/published.jar"),
            ]
        );
    }

    #[test]
    fn project_classpath_preserves_order_without_declared_outputs() {
        let mut module = Module::new(ModuleId::new(":explicit", "main"), "/workspace");
        module.classpath = vec![
            PathBuf::from("/cache/selected.jar"),
            PathBuf::from("/workspace/build/classes"),
        ];
        let model =
            ProjectModel::new("/workspace", ProviderKind::Explicit).with_modules(vec![module]);

        assert_eq!(
            synced_project_classpath(model),
            vec![
                PathBuf::from("/cache/selected.jar"),
                PathBuf::from("/workspace/build/classes"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_classpath_matches_a_declared_output_through_a_symlink() {
        use std::os::unix::fs::symlink;

        let tree = TempTree::new("sync-output-symlink");
        let workspace = tree.path("workspace");
        let output = tree.path("composite/classes");
        let linked_output = workspace.join("linked-classes");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&output).unwrap();
        symlink(&output, &linked_output).unwrap();

        let mut dependency = Module::new(ModuleId::new(":dependency", "main"), &workspace);
        dependency.outputs = vec![ModuleOutput::location(output)];
        let mut consumer = Module::new(ModuleId::new(":consumer", "main"), &workspace);
        consumer.classpath = vec![PathBuf::from("/cache/published.jar"), linked_output.clone()];
        let model = ProjectModel::new(&workspace, ProviderKind::Gradle)
            .with_modules(vec![consumer, dependency]);

        assert_eq!(
            synced_project_classpath(model),
            vec![linked_output, PathBuf::from("/cache/published.jar")]
        );
    }

    #[test]
    fn project_classpath_memoizes_duplicate_paths() {
        use std::cell::Cell;

        let mut dependency = Module::new(ModuleId::new(":dependency", "main"), "/workspace");
        let mut consumer = Module::new(ModuleId::new(":consumer", "main"), "/workspace");
        for index in 0..128 {
            let output = PathBuf::from(format!("/declared/output-{index}"));
            let alias = PathBuf::from(format!("/alias/output-{index}"));
            dependency.outputs.extend([
                ModuleOutput::location(output.clone()),
                ModuleOutput::location(output.clone()),
            ]);
            consumer
                .friend_paths
                .extend([output.clone(), output.clone()]);
            consumer.classpath.extend([alias.clone(), alias]);
        }
        let model = ProjectModel::new("/workspace", ProviderKind::Gradle)
            .with_modules(vec![consumer, dependency]);
        let mut sync =
            ProjectSync::new(Box::new(ScriptedProvider::new(Vec::new(), vec![Ok(model)])));
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );
        let calls = Cell::new(0usize);
        let classpath = sync.project_classpath_with(&|path| {
            calls.set(calls.get() + 1);
            let name = path.file_name()?.to_str()?;
            Some(PathBuf::from("/canonical").join(name))
        });

        assert_eq!(classpath.len(), 256);
        assert_eq!(calls.get(), 256);
    }

    #[test]
    fn accepted_snapshot_change_invalidates_project_sources() {
        let tree = TempTree::new("sync-source-cache");
        tree.write(
            "build.gradle.kts",
            "dependencies { implementation(project(\":dep\")) }",
        );
        tree.write("consumer/Open.kt", "fun open() = support()");
        tree.write("dependency/Support.kt", "fun support() = 1");
        let consumer_root = tree.path("consumer");
        let dependency_root = tree.path("dependency");
        let dependency_id = ModuleId::new(":dependency", "main");
        let model = |has_dependency| {
            let mut consumer = Module::new(ModuleId::new(":consumer", "main"), &consumer_root);
            consumer.source_roots = vec![SourceRoot::source(&consumer_root)];
            if has_dependency {
                consumer.depends_on = vec![dependency_id.clone()];
            }
            let mut dependency = Module::new(dependency_id.clone(), &dependency_root);
            dependency.source_roots = vec![SourceRoot::source(&dependency_root)];
            ProjectModel::new(tree.root(), ProviderKind::Gradle)
                .with_modules(vec![consumer, dependency])
        };
        let mut sync = ProjectSync::new(Box::new(ScriptedProvider::new(
            vec![tree.path("build.gradle.kts")],
            vec![Ok(model(true)), Ok(model(false))],
        )));
        let open_uri = url::Url::from_file_path(tree.path("consumer/Open.kt"))
            .unwrap()
            .to_string();
        let documents = [(open_uri.as_str(), "fun open() = support()")];
        let open_uris = [open_uri.as_str()];
        let mut sources = ProjectSources::default();

        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );
        let first = sources
            .load(
                sync.snapshot().unwrap(),
                &documents,
                &open_uris,
                32 * 1024 * 1024,
            )
            .unwrap()
            .0
            .to_vec();
        tree.write("build.gradle.kts", "dependencies {}");
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );
        let second = sources
            .load(
                sync.snapshot().unwrap(),
                &documents,
                &open_uris,
                32 * 1024 * 1024,
            )
            .unwrap()
            .0
            .to_vec();

        assert_eq!(
            first,
            [(
                url::Url::from_file_path(tree.path("dependency/Support.kt"))
                    .unwrap()
                    .to_string(),
                "fun support() = 1".to_string(),
            )]
        );
        assert!(second.is_empty());
    }

    #[test]
    fn a_failed_probe_keeps_the_last_good_model_serving() {
        let tree = TempTree::new("sync-failure");
        tree.write("build.gradle.kts", "dependencies {}");
        let mut sync = ProjectSync::new(Box::new(ScriptedProvider::new(
            vec![tree.path("build.gradle.kts")],
            vec![
                Ok(model_with("/m2/a.jar")),
                Err(ProbeError::Parse("broken".to_string())),
            ],
        )));

        sync.refresh(&FakeRunner::default());
        tree.write("build.gradle.kts", "dependencies { oops");
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Failed {
                error: ProbeError::Parse("broken".to_string()),
                model_retained: true,
            }
        );
        assert_eq!(sync.project_classpath(), vec![PathBuf::from("/m2/a.jar")]);
    }

    #[test]
    fn rolling_back_a_model_restores_the_last_good_classpath_and_retries() {
        let tree = TempTree::new("sync-rollback");
        tree.write("build.gradle.kts", "dependencies {}");
        let mut sync = ProjectSync::new(Box::new(ScriptedProvider::new(
            vec![tree.path("build.gradle.kts")],
            vec![
                Ok(model_with("/m2/a.jar")),
                Ok(model_with("/m2/b.jar")),
                Ok(model_with("/m2/c.jar")),
            ],
        )));

        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );
        let previous = sync.snapshot().cloned();
        tree.write("build.gradle.kts", "dependencies { implementation(b) }");
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );

        sync.rollback_snapshot(previous);
        assert_eq!(sync.project_classpath(), vec![PathBuf::from("/m2/a.jar")]);
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );
        assert_eq!(sync.project_classpath(), vec![PathBuf::from("/m2/c.jar")]);
    }

    #[test]
    fn a_failed_probe_is_retried_rather_than_cached_as_the_project_state() {
        let tree = TempTree::new("sync-retry");
        tree.write("build.gradle.kts", "dependencies {}");
        let provider = ScriptedProvider::new(
            vec![tree.path("build.gradle.kts")],
            vec![
                Err(ProbeError::Parse("first".to_string())),
                Ok(model_with("/m2/a.jar")),
            ],
        );
        let mut sync = ProjectSync::new(Box::new(provider));

        assert!(matches!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Failed {
                model_retained: false,
                ..
            }
        ));
        // Same content, but the previous attempt produced no model: refresh must try again.
        assert!(matches!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        ));
    }

    #[test]
    fn changing_provider_reprobes_without_discarding_the_last_good_model() {
        let mut sync = ProjectSync::new(Box::new(ScriptedProvider::new(
            Vec::new(),
            vec![Ok(model_with("/m2/a.jar"))],
        )));
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );

        sync.update_provider(Box::new(
            ScriptedProvider::new(
                Vec::new(),
                vec![Err(ProbeError::Parse("broken pom".to_string()))],
            )
            .with_kind(ProviderKind::Maven),
        ));

        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Failed {
                error: ProbeError::Parse("broken pom".to_string()),
                model_retained: true,
            }
        );
        assert_eq!(sync.project_classpath(), vec![PathBuf::from("/m2/a.jar")]);
    }

    #[test]
    fn adding_a_local_jar_changes_the_no_build_system_model() {
        let tree = TempTree::new("sync-local-jar");
        let mut sync = ProjectSync::new(Box::new(
            super::super::provider::NoBuildSystemProvider::new(tree.root()),
        ));

        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );
        assert!(sync.project_classpath().is_empty());

        tree.write("libs/support.jar", "");
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );
        assert_eq!(
            sync.project_classpath(),
            vec![tree.path("libs/support.jar")]
        );
    }

    #[test]
    fn each_change_restarts_the_debounce_window() {
        let mut sync = ProjectSync::new(Box::new(ScriptedProvider::new(Vec::new(), Vec::new())));
        assert_eq!(sync.refresh_due_in(0), None);

        sync.note_change(1_000);
        sync.note_change(1_400);
        assert_eq!(sync.refresh_due_in(1_750), Some(400));
        assert!(!sync.take_due(2_149));
        assert!(sync.take_due(2_150));
        assert_eq!(sync.refresh_due_in(3_000), None);
    }
}
