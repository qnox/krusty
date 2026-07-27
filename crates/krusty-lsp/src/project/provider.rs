//! What every project-model producer implements.

use std::fmt;
use std::path::{Path, PathBuf};

use super::model::{Module, ModuleId, ProjectModel, ProviderKind, SourceRoot};
use super::runner::CommandRunner;

const PROJECT_WATCH_GLOBS: &[&str] = &[
    "**/.bsp/*.json",
    "**/*.gradle",
    "**/*.gradle.kts",
    "**/gradle.properties",
    "**/libs.versions.toml",
    "**/gradle-wrapper.properties",
    "**/gradle.lockfile",
    "**/pom.xml",
    "**/.mvn/**",
    "**/lib/*.jar",
    "**/libs/*.jar",
    "**/BUILD",
    "**/BUILD.bazel",
    "**/*.bzl",
    "**/build.sbt",
    "**/build.sc",
    "**/.idea/modules.xml",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeError {
    /// The build tool could not be started, or a file it was expected to write is missing.
    Io(String),
    /// The build tool ran and failed.
    Tool {
        program: String,
        status: i32,
        message: String,
    },
    /// The build tool succeeded but its output could not be understood.
    Parse(String),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeError::Io(message) => write!(formatter, "{message}"),
            ProbeError::Tool {
                program,
                status,
                message,
            } => {
                write!(formatter, "{program} exited with {status}")?;
                let detail = message.trim();
                if detail.is_empty() {
                    Ok(())
                } else {
                    write!(formatter, ": {detail}")
                }
            }
            ProbeError::Parse(message) => write!(formatter, "unreadable build output: {message}"),
        }
    }
}

pub trait ProjectProvider {
    fn kind(&self) -> ProviderKind;
    fn root(&self) -> &Path;

    fn watch_paths(&self) -> Vec<PathBuf>;

    fn fingerprint_salt(&self) -> String {
        format!("{}-v1", self.kind().as_str())
    }

    fn additional_watch_globs(&self) -> &'static [&'static str] {
        &[]
    }

    fn watch_globs(&self) -> Vec<String> {
        if self.kind() == ProviderKind::Explicit {
            return Vec::new();
        }
        PROJECT_WATCH_GLOBS
            .iter()
            .chain(self.additional_watch_globs())
            .map(|glob| glob.to_string())
            .collect()
    }

    fn probe(&self, runner: &dyn CommandRunner) -> Result<ProjectModel, ProbeError>;
}

/// A classpath handed to `krusty-lsp` on the command line. The user's word wins: no build tool runs.
#[derive(Debug)]
pub struct ExplicitProvider {
    root: PathBuf,
    classpath: Vec<PathBuf>,
}

impl ExplicitProvider {
    pub fn new(root: impl Into<PathBuf>, classpath: Vec<PathBuf>) -> Self {
        Self {
            root: root.into(),
            classpath,
        }
    }
}

impl ProjectProvider for ExplicitProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Explicit
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn probe(&self, _runner: &dyn CommandRunner) -> Result<ProjectModel, ProbeError> {
        Ok(ProjectModel::new(self.root.clone(), ProviderKind::Explicit)
            .with_modules(vec![single_module(&self.root, self.classpath.clone())]))
    }
}

/// No build system was found. Sources are whatever lies under the root; the classpath is any jar in
/// the conventional local library directories.
#[derive(Debug)]
pub struct NoBuildSystemProvider {
    root: PathBuf,
}

impl NoBuildSystemProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn local_jars(&self) -> Vec<PathBuf> {
        let mut jars = Vec::new();
        for directory in ["lib", "libs"] {
            let Ok(entries) = std::fs::read_dir(self.root.join(directory)) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|extension| extension == "jar") {
                    jars.push(path);
                }
            }
        }
        jars.sort();
        jars
    }
}

impl ProjectProvider for NoBuildSystemProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::None
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        self.local_jars()
    }

    fn probe(&self, _runner: &dyn CommandRunner) -> Result<ProjectModel, ProbeError> {
        Ok(ProjectModel::new(self.root.clone(), ProviderKind::None)
            .with_modules(vec![single_module(&self.root, self.local_jars())]))
    }
}

fn single_module(root: &Path, classpath: Vec<PathBuf>) -> Module {
    let mut module = Module::new(ModuleId::new(":", "main"), root.to_path_buf());
    module.display_name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());
    module.source_roots = vec![SourceRoot::source(root.to_path_buf())];
    module.classpath = classpath;
    module
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::runner::testing::FakeRunner;
    use crate::project::testing::TempTree;

    #[test]
    fn an_explicit_classpath_never_runs_a_build_tool() {
        let runner = FakeRunner::default();
        let provider = ExplicitProvider::new("/p", vec![PathBuf::from("/m2/a.jar")]);
        let model = provider.probe(&runner).unwrap();

        assert_eq!(runner.command_count(), 0);
        assert!(provider.watch_paths().is_empty());
        assert_eq!(model.kind, ProviderKind::Explicit);
        assert_eq!(model.modules[0].classpath, vec![PathBuf::from("/m2/a.jar")]);
        assert_eq!(model.modules[0].source_roots[0].path, PathBuf::from("/p"));
    }

    #[test]
    fn without_a_build_system_local_jar_directories_form_the_classpath() {
        let tree = TempTree::new("provider-none");
        tree.write("libs/support.jar", "");
        tree.write("lib/core.jar", "");
        tree.write("lib/notes.txt", "");

        let model = NoBuildSystemProvider::new(tree.root())
            .probe(&FakeRunner::default())
            .unwrap();
        assert_eq!(
            model.modules[0].classpath,
            vec![tree.path("lib/core.jar"), tree.path("libs/support.jar")]
        );
    }

    #[test]
    fn probe_errors_render_for_a_single_line_editor_warning() {
        assert_eq!(
            ProbeError::Tool {
                program: "gradle".to_string(),
                status: 1,
                message: "\nFAILURE: build failed\n".to_string(),
            }
            .to_string(),
            "gradle exited with 1: FAILURE: build failed"
        );
    }
}
