//! Source roots, classpath, module graph, and JDK for one worktree.

use std::path::{Path, PathBuf};

/// Stable identity of one compilation unit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(String);

impl ModuleId {
    pub fn new(module: &str, source_set: &str) -> Self {
        Self(format!("{module}:{source_set}"))
    }

    /// An id whose form is decided by the producer (e.g. a BSP build-target URI), used verbatim.
    pub fn raw(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether a source root holds production or test sources, and whether a build step produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceRootKind {
    Source,
    Test,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRoot {
    pub path: PathBuf,
    pub kind: SourceRootKind,
    pub generated: bool,
}

impl SourceRoot {
    pub fn source(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: SourceRootKind::Source,
            generated: false,
        }
    }

    pub fn test(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: SourceRootKind::Test,
            generated: false,
        }
    }

    pub fn generated(mut self) -> Self {
        self.generated = true;
        self
    }
}

/// One compilation unit of the project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Module {
    pub id: Option<ModuleId>,
    pub display_name: String,
    pub base_directory: PathBuf,
    pub source_roots: Vec<SourceRoot>,
    /// Resolved compile classpath: jars and class directories, in build-tool order.
    pub classpath: Vec<PathBuf>,
    pub output_dir: Option<PathBuf>,
    pub depends_on: Vec<ModuleId>,
    /// BSP `associates`: outputs whose `internal` declarations this module may see.
    pub friend_paths: Vec<PathBuf>,
    pub jvm_target: Option<String>,
    pub kotlinc_args: Vec<String>,
}

impl Module {
    pub fn new(id: ModuleId, base_directory: impl Into<PathBuf>) -> Self {
        Self {
            display_name: id.as_str().to_string(),
            id: Some(id),
            base_directory: base_directory.into(),
            ..Self::default()
        }
    }
}

/// Which producer built the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    /// A classpath was passed on the command line; no build tool is consulted.
    Explicit,
    /// A Build Server Protocol server advertised via `.bsp/*.json`.
    Bsp,
    Gradle,
    Maven,
    /// No build system was found: builtins, the JDK, and any local jar directories.
    None,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Explicit => "explicit",
            ProviderKind::Bsp => "bsp",
            ProviderKind::Gradle => "gradle",
            ProviderKind::Maven => "maven",
            ProviderKind::None => "none",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectModel {
    pub root: PathBuf,
    pub kind: ProviderKind,
    pub jdk_home: Option<PathBuf>,
    pub modules: Vec<Module>,
}

impl ProjectModel {
    pub fn new(root: impl Into<PathBuf>, kind: ProviderKind) -> Self {
        Self {
            root: root.into(),
            kind,
            jdk_home: None,
            modules: Vec::new(),
        }
    }

    pub fn with_modules(mut self, modules: Vec<Module>) -> Self {
        self.modules = modules;
        self
    }

    pub fn module(&self, id: &ModuleId) -> Option<&Module> {
        self.modules
            .iter()
            .find(|module| module.id.as_ref() == Some(id))
    }

    pub fn module_for_source(&self, path: &Path) -> Option<&Module> {
        self.modules
            .iter()
            .filter_map(|module| {
                module
                    .source_roots
                    .iter()
                    .filter(|root| path.starts_with(&root.path))
                    .map(|root| root.path.components().count())
                    .max()
                    .map(|depth| (depth, module))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, module)| module)
    }

    /// Classpath handed to the compiler for `module`: its own classpath, its friend paths, and the
    /// output of everything it depends on. Deduplicated, first occurrence wins.
    pub fn compile_classpath(&self, module: &Module) -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = Vec::new();
        let push = |entry: &Path, entries: &mut Vec<PathBuf>| {
            if !entries.iter().any(|existing| existing == entry) {
                entries.push(entry.to_path_buf());
            }
        };
        for entry in &module.classpath {
            push(entry, &mut entries);
        }
        for entry in &module.friend_paths {
            push(entry, &mut entries);
        }
        for dependency in &module.depends_on {
            if let Some(output) = self
                .module(dependency)
                .and_then(|m| m.output_dir.as_deref())
            {
                push(output, &mut entries);
            }
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ProjectModel {
        let mut core = Module::new(ModuleId::new(":core", "main"), "/p/core");
        core.source_roots = vec![SourceRoot::source("/p/core/src/main/kotlin")];
        core.output_dir = Some(PathBuf::from("/p/core/build/classes/kotlin/main"));
        core.classpath = vec![PathBuf::from("/m2/kotlin-stdlib.jar")];

        let mut app = Module::new(ModuleId::new(":app", "main"), "/p/app");
        app.source_roots = vec![SourceRoot::source("/p/app/src/main/kotlin")];
        app.output_dir = Some(PathBuf::from("/p/app/build/classes/kotlin/main"));
        app.classpath = vec![PathBuf::from("/m2/kotlin-stdlib.jar")];
        app.depends_on = vec![ModuleId::new(":core", "main")];

        let mut app_test = Module::new(ModuleId::new(":app", "test"), "/p/app");
        app_test.source_roots = vec![SourceRoot::test("/p/app/src/test/kotlin")];
        app_test.depends_on = vec![ModuleId::new(":app", "main")];
        app_test.friend_paths = vec![PathBuf::from("/p/app/build/classes/kotlin/main")];

        ProjectModel::new("/p", ProviderKind::Gradle).with_modules(vec![core, app, app_test])
    }

    #[test]
    fn compile_classpath_adds_friend_paths_and_dependency_output_without_duplicates() {
        let model = model();
        let app = model.module(&ModuleId::new(":app", "main")).unwrap();
        assert_eq!(
            model.compile_classpath(app),
            vec![
                PathBuf::from("/m2/kotlin-stdlib.jar"),
                PathBuf::from("/p/core/build/classes/kotlin/main"),
            ]
        );

        let test = model.module(&ModuleId::new(":app", "test")).unwrap();
        // The friend path and the dependency output are the same directory: it appears once.
        assert_eq!(
            model.compile_classpath(test),
            vec![PathBuf::from("/p/app/build/classes/kotlin/main")]
        );
    }

    #[test]
    fn source_paths_select_the_module_with_the_most_specific_root() {
        let model = model();

        assert_eq!(
            model
                .module_for_source(Path::new("/p/app/src/test/kotlin/Example.kt"))
                .and_then(|module| module.id.as_ref())
                .map(ModuleId::as_str),
            Some(":app:test")
        );
        assert!(model
            .module_for_source(Path::new("/p/unowned/Example.kt"))
            .is_none());
    }
}
