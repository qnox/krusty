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

use std::path::PathBuf;

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
}
