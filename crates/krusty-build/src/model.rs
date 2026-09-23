//! What a build needs to know about one module.
//!
//! This is deliberately wider than `crates/krusty-lsp/src/project/model.rs`, whose `Module` exists
//! to feed an analyzer a classpath. `docs/PROJECT_MODEL.md` is explicit that the analysis worker
//! consumes "the union of all module classpaths" — the opposite of the per-module isolation a build
//! requires. Lifting that model is therefore an extension, not a move. The fields below marked
//! *(absent in the LSP model)* are what has to be added:
//!
//! * `resources` — `project/jps.rs` skips resource folders outright.
//! * `module_name` — reaches the emitted bytes three ways: `@Metadata.classModuleName`, the
//!   `META-INF/<module>.kotlin_module` file name, and kotlinx.serialization's
//!   `write$Self$<module>` mangling.
//! * `jdk_home` — per-module, since the bootclasspath decides what `java.*` resolves to.
//! * `java_sources` — Gradle modules routinely mix Java, and krusty has no Java frontend, so the
//!   driver must know to refuse or delegate rather than silently emit a short jar.
//! * `processor_path` / `processor_options` — KSP processors are external jars that GENERATE
//!   sources, so their identity belongs in the cache key.
//! * `ModuleOutput::kind` — a jar and a class directory are not interchangeable to a consumer.

use std::path::{Component, Path, PathBuf};

/// Stable identity of a module, as its build system names it (`:app:main`, `lib/test`).
///
/// Opaque and compared by string: it comes from a build tool and is never parsed here.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(String);

impl ModuleId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ModuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a source root holds production or test sources. They are separate modules with separate
/// classpaths; a test module sees its main module through `friend_paths` so `internal` resolves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceRootKind {
    Main,
    Test,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRoot {
    pub path: PathBuf,
    /// Build-model classification only. Main and test source sets are separate modules; once a
    /// module reaches the compiler this tag does not change emitted bytes.
    pub kind: SourceRootKind,
    /// Generated roots (KSP output, build-time codegen) are rebuilt rather than edited, and a
    /// driver may need to produce them before this module compiles.
    pub generated: bool,
}

/// Where a module's compiled output goes, and in what shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleOutput {
    ClassDirectory(PathBuf),
    Jar(PathBuf),
}

impl ModuleOutput {
    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::ClassDirectory(p) | Self::Jar(p) => p,
        }
    }
}

/// One compilation unit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Module {
    pub id: Option<ModuleId>,
    pub display_name: String,
    pub base_directory: PathBuf,

    pub source_roots: Vec<SourceRoot>,
    /// Resource directories copied into the output. *(absent in the LSP model)*
    pub resources: Vec<PathBuf>,
    /// Java sources found under this module's roots. krusty has no Java frontend, so a non-empty
    /// list means the driver must refuse or delegate. *(absent in the LSP model)*
    pub java_sources: Vec<PathBuf>,

    /// Compile classpath, in build-tool order. Order is preserved because it is load-bearing:
    /// kotlinx.serialization picks its `write$Self` mangling by parsing a jar's FILE NAME
    /// (`SerializationAbi::from_classpath`), so classpath identity is not only content identity.
    pub classpath: Vec<PathBuf>,
    /// Outputs whose `internal` declarations this module may see (BSP `associates`). Friendship is
    /// exact path-set membership in `src/jvm/classpath.rs`, so these are identity, not content.
    pub friend_paths: Vec<PathBuf>,
    pub depends_on: Vec<ModuleId>,
    pub outputs: Vec<ModuleOutput>,

    /// kotlinc `-module-name`. Reaches the emitted bytes. *(absent in the LSP model)*
    pub module_name: Option<String>,
    /// Per-module JDK; its bootclasspath decides what `java.*` resolves to. *(absent in the LSP
    /// model)*
    pub jdk_home: Option<PathBuf>,
    pub jvm_target: Option<String>,
    pub kotlinc_args: Vec<String>,

    /// Annotation/symbol processor jars. They generate sources, so their identity is a compile
    /// input. *(absent in the LSP model)*
    pub processor_path: Vec<PathBuf>,
    /// Processor options, as `key=value`, in declaration order. *(absent in the LSP model)*
    pub processor_options: Vec<String>,
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

    /// Whether this module can be compiled by krusty at all. A module with Java sources cannot:
    /// there is no Java frontend, and emitting the Kotlin half alone produces a jar that is missing
    /// classes with no diagnostic.
    pub fn is_krusty_compilable(&self) -> bool {
        self.java_sources.is_empty()
    }

    /// Resolve every module-owned relative path against its absolute base directory.
    ///
    /// This runs at graph insertion, before ownership checks, cache keys, or compilation. No path
    /// in a planned graph therefore depends on the build process's current working directory.
    pub(crate) fn normalize_paths(&mut self) -> Result<(), PathBuf> {
        if !self.base_directory.is_absolute() {
            return Err(self.base_directory.clone());
        }
        self.base_directory = lexical_normalize(&self.base_directory);
        let base = self.base_directory.clone();
        let resolve = |path: &mut PathBuf| {
            let resolved = if path.is_absolute() {
                path.clone()
            } else {
                base.join(&*path)
            };
            *path = lexical_normalize(&resolved);
        };
        for root in &mut self.source_roots {
            resolve(&mut root.path);
        }
        for path in self
            .resources
            .iter_mut()
            .chain(&mut self.java_sources)
            .chain(&mut self.classpath)
            .chain(&mut self.friend_paths)
            .chain(&mut self.processor_path)
        {
            resolve(path);
        }
        for output in &mut self.outputs {
            match output {
                ModuleOutput::ClassDirectory(path) | ModuleOutput::Jar(path) => resolve(path),
            }
        }
        if let Some(jdk_home) = &mut self.jdk_home {
            resolve(jdk_home);
        }
        Ok(())
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::ParentDir) | None if !path.has_root() => {
                    normalized.push("..");
                }
                Some(Component::Prefix(_))
                | Some(Component::RootDir)
                | Some(Component::ParentDir)
                | None => {}
                Some(Component::CurDir) => unreachable!("current-directory parts are skipped"),
            },
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_with_java_sources_is_not_krusty_compilable() {
        let mut module = Module::new(ModuleId::new(":app:main"), "/repo/app");
        assert!(module.is_krusty_compilable());
        module.java_sources.push(PathBuf::from("/repo/app/A.java"));
        assert!(
            !module.is_krusty_compilable(),
            "krusty has no Java frontend; compiling the Kotlin half alone yields a short jar"
        );
    }

    #[test]
    fn module_id_round_trips_and_displays() {
        let id = ModuleId::new(":lib:test");
        assert_eq!(id.as_str(), ":lib:test");
        assert_eq!(id.to_string(), ":lib:test");
    }

    #[test]
    fn output_path_is_readable_for_both_shapes() {
        assert_eq!(
            ModuleOutput::ClassDirectory(PathBuf::from("/out/classes")).path(),
            std::path::Path::new("/out/classes")
        );
        assert_eq!(
            ModuleOutput::Jar(PathBuf::from("/out/lib.jar")).path(),
            std::path::Path::new("/out/lib.jar")
        );
    }

    #[test]
    fn relative_paths_resolve_against_the_module_base_not_process_cwd() {
        let mut module = Module::new(ModuleId::new("app"), "/workspace/app");
        module.source_roots.push(SourceRoot {
            path: PathBuf::from("src/./main/../main"),
            kind: SourceRootKind::Main,
            generated: false,
        });
        module.classpath.push(PathBuf::from("../lib/api.jar"));
        module
            .outputs
            .push(ModuleOutput::ClassDirectory(PathBuf::from("out/classes")));

        module.normalize_paths().expect("absolute module base");
        assert_eq!(
            module.source_roots[0].path,
            Path::new("/workspace/app/src/main")
        );
        assert_eq!(module.classpath[0], Path::new("/workspace/lib/api.jar"));
        assert_eq!(
            module.outputs[0].path(),
            Path::new("/workspace/app/out/classes")
        );
        assert!(module.source_roots[0].path.is_absolute());
    }

    #[test]
    fn normalization_is_identical_from_different_process_working_directories() {
        let root = std::env::temp_dir().join(format!(
            "krusty-build-cwd-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).expect("first cwd");
        std::fs::create_dir_all(&second).expect("second cwd");
        for cwd in [&first, &second] {
            let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "model::tests::normalization_child_ignores_process_cwd",
                ])
                .env("KRUSTY_BUILD_CWD_CHILD", "1")
                .current_dir(cwd)
                .status()
                .expect("run child test");
            assert!(
                status.success(),
                "normalization failed from {}",
                cwd.display()
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn normalization_child_ignores_process_cwd() {
        if std::env::var_os("KRUSTY_BUILD_CWD_CHILD").is_none() {
            return;
        }
        let mut module = Module::new(ModuleId::new("app"), "/workspace/app");
        module.classpath.push(PathBuf::from("../lib/api.jar"));
        module.normalize_paths().expect("absolute base");
        assert_eq!(module.classpath, [PathBuf::from("/workspace/lib/api.jar")]);
    }

    #[test]
    fn a_relative_module_base_is_rejected_before_paths_reach_the_cwd() {
        let mut module = Module::new(ModuleId::new("app"), "relative/app");
        assert_eq!(module.normalize_paths(), Err(PathBuf::from("relative/app")));
    }

    #[test]
    fn resolving_parent_components_cannot_escape_the_absolute_root() {
        let mut module = Module::new(ModuleId::new("app"), "/workspace/app");
        module.classpath.push(PathBuf::from("../../../api.jar"));
        module.normalize_paths().expect("absolute base");
        assert_eq!(module.classpath, [PathBuf::from("/api.jar")]);
        assert!(module.classpath[0].is_absolute());
    }
}
