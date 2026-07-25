//! Content-guarded project-model refresh with last-good retention.

use std::path::PathBuf;

use super::fingerprint::{fingerprint_files, Fingerprint};
use super::model::{ProjectModel, ProviderKind};
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
    model: Option<ProjectModel>,
    fingerprint: Option<Fingerprint>,
    dirty_since: Option<u64>,
}

impl ProjectSync {
    pub fn new(provider: Box<dyn ProjectProvider>) -> Self {
        Self {
            provider,
            model: None,
            fingerprint: None,
            dirty_since: None,
        }
    }

    pub fn kind(&self) -> ProviderKind {
        self.provider.kind()
    }

    pub fn model(&self) -> Option<&ProjectModel> {
        self.model.as_ref()
    }

    pub fn rollback_model(&mut self, model: Option<ProjectModel>) {
        self.model = model;
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
        if self.model.is_some() && self.fingerprint == Some(fingerprint) {
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
                    model_retained: self.model.is_some(),
                };
            }
        };
        match self.provider.probe(runner) {
            Ok(model) => {
                self.fingerprint = Some(fingerprint);
                self.model = Some(model);
                RefreshOutcome::Updated
            }
            Err(error) => RefreshOutcome::Failed {
                error,
                model_retained: self.model.is_some(),
            },
        }
    }

    /// The union of every module's compile classpath.
    ///
    /// The compiler worker analyses all open documents as one source set against a single
    /// classpath, so this is what the whole session is configured with. It is a superset of any one
    /// module's classpath, which keeps cross-module references resolving at the cost of admitting a
    /// few symbols a stricter per-module view would reject.
    pub fn project_classpath(&self) -> Vec<PathBuf> {
        let Some(model) = self.model.as_ref() else {
            return Vec::new();
        };
        let mut union: Vec<PathBuf> = Vec::new();
        for module in &model.modules {
            for entry in model.compile_classpath(module) {
                if !union.contains(&entry) {
                    union.push(entry);
                }
            }
        }
        union
    }

    /// The `jvmTarget` the project reports, used to pick a matching JDK.
    pub fn jvm_target(&self) -> Option<&str> {
        self.model
            .as_ref()?
            .modules
            .iter()
            .find_map(|module| module.jvm_target.as_deref())
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
    use crate::project::model::{Module, ModuleId, SourceRoot};
    use crate::project::runner::testing::FakeRunner;
    use crate::project::testing::TempTree;

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
        let previous = sync.model().cloned();
        tree.write("build.gradle.kts", "dependencies { implementation(b) }");
        assert_eq!(
            sync.refresh(&FakeRunner::default()),
            RefreshOutcome::Updated
        );

        sync.rollback_model(previous);
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
