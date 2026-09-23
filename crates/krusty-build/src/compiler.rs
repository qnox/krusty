//! A [`BuildEnvironment`] that drives the real `krusty` binary, one process per module.
//!
//! Process-per-module is not a convenience here, it is forced: compiler state deliberately holds
//! `Rc`/`RefCell` so it is not `Send`, and interning leaks for the life of the process (which is why
//! the language server restarts its worker every 64 analyses). A thread-per-module driver is not
//! available without redesigning both. Spawning the binary also gives crash isolation for free —
//! `docs/PROJECT_PARITY.md` records 10 SIGBUS crashes across 931 real modules, and a crashed module
//! must fail its own build step rather than take the whole build down.
//!
//! # Determinism
//!
//! Sources are compiled in one canonical order — sorted by path — and the SAME order is what
//! [`KrustyCli::base_inputs`] puts in the cache key. That matters because source order is
//! observable in emitted output: a package's facade-name list accumulates in file-streaming order,
//! so two orders produce different `.kotlin_module` bytes
//! (`tests/emission_determinism_e2e.rs::module_facade_order_follows_source_order_but_classes_do_not`).
//! Picking an order and keying it is what makes the cache sound here; keying a sorted multiset of
//! contents would not be.

use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::jvm::compilation_inputs::JvmCompilationInputInventory;

use crate::cache::{CacheKeyInputs, FileDigest};
use crate::driver::{BuildEnvironment, CompiledModule, PublishedAbi};
use crate::model::Module;

/// Disable toolchain discovery whose selected bytes are not represented by `Module::classpath`.
/// The ambient JDK selection is resolved once, keyed by exact `lib/modules` contents, and then
/// supplied as an explicit classpath entry while `-no-jdk` keeps the subprocess from reselecting it.
const HERMETIC_CLASSPATH_ARGS: &[&str] = &["-no-stdlib", "-no-reflect", "-no-jdk"];

/// Drives `krusty` as a subprocess.
#[derive(Clone, Debug)]
pub struct KrustyCli {
    binary: PathBuf,
    /// Exact compiler identity for the cache key. Construction records an error rather than
    /// inventing an identity for an unreadable executable; `base_inputs` then fails before lookup.
    compiler_identity: Result<String, String>,
    /// Exact identity of the JDK image passed explicitly while ambient discovery is disabled.
    jdk_identity: Result<String, String>,
    /// JDK image selected at construction and passed explicitly while ambient discovery is off.
    jdk_modules: Option<PathBuf>,
    /// Flags passed to every module, after the module's own `kotlinc_args`.
    common_args: Vec<String>,
    /// Scratch directory for per-module output before it is read back.
    scratch: PathBuf,
}

impl KrustyCli {
    pub fn new(binary: impl Into<PathBuf>, scratch: impl Into<PathBuf>) -> Self {
        let binary = binary.into();
        let inputs = JvmCompilationInputInventory::from_explicit_classpath_and_jdk(&[], None);
        let jdk_modules = inputs.jdk_modules().map(Path::to_path_buf);
        Self {
            compiler_identity: identify_compiler(&binary),
            jdk_identity: identify_jdk(jdk_modules.as_deref()),
            jdk_modules,
            binary,
            common_args: Vec::new(),
            scratch: scratch.into(),
        }
    }

    pub fn with_common_args(mut self, args: Vec<String>) -> Self {
        self.common_args = args;
        self
    }

    /// A module's Kotlin sources, sorted by path — the canonical compile order. See module docs.
    ///
    /// An unreadable root or entry is an ERROR, never a shorter list. A partial source set would
    /// compile to a partial module and then be cached under a key that looks perfectly valid, which
    /// is the same silent-wrongness the total-output contract exists to prevent.
    pub fn sources_of(module: &Module) -> Result<Vec<PathBuf>, String> {
        let mut sources = Vec::new();
        for root in &module.source_roots {
            collect_kotlin(&root.path, &mut sources)?;
        }
        sources.sort();
        Ok(sources)
    }

    /// Entries that enter the CACHE KEY: the module's own classpath plus its friend paths. These
    /// are external to the build graph, so their content identity is what matters.
    fn keyed_classpath(&self, module: &Module) -> Vec<PathBuf> {
        let mut entries = module.classpath.clone();
        entries.extend(module.friend_paths.iter().cloned());
        entries
    }

    /// The full compile classpath: the keyed entries plus the dependency output directories.
    ///
    /// Dependency outputs come last so a module's declared classpath wins a name collision, which
    /// is the order a build tool hands them over in. They are on the classpath but NOT in the key —
    /// see [`BuildEnvironment::compile`] for why digesting them would defeat avoidance.
    fn compile_classpath(&self, module: &Module, dependency_outputs: &[PathBuf]) -> Vec<PathBuf> {
        let mut entries = self.keyed_classpath(module);
        entries.extend(dependency_outputs.iter().cloned());
        entries.extend(self.jdk_modules.iter().cloned());
        entries
    }

    fn module_args(
        &self,
        module: &Module,
        sources: &[PathBuf],
        output: &Path,
        dependency_outputs: &[PathBuf],
    ) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        for source in sources {
            args.push(source.display().to_string());
        }
        let classpath = self.compile_classpath(module, dependency_outputs);
        if !classpath.is_empty() {
            args.push("-cp".into());
            args.push(join_classpath(&classpath));
        }
        if !module.friend_paths.is_empty() {
            args.push(format!(
                "-Xfriend-paths={}",
                join_classpath(&module.friend_paths)
            ));
        }
        if let Some(name) = &module.module_name {
            args.push("-module-name".into());
            args.push(name.clone());
        }
        if let Some(target) = &module.jvm_target {
            args.push("-jvm-target".into());
            args.push(target.clone());
        }
        args.extend(module.kotlinc_args.iter().cloned());
        args.extend(self.common_args.iter().cloned());
        args.extend(HERMETIC_CLASSPATH_ARGS.iter().map(|arg| (*arg).into()));
        args.push("-d".into());
        args.push(output.display().to_string());
        args
    }
}

impl BuildEnvironment for KrustyCli {
    /// Fields the model can express that this environment does not implement.
    ///
    /// Each is refused rather than ignored. Ignoring them produces output that is wrong in a way
    /// nothing downstream can detect: a module built against the wrong JDK, a jar missing its
    /// resources, or a module whose generated sources were never generated — and then that output
    /// is cached under a key that looks entirely valid.
    fn unsupported(&self, module: &Module) -> Option<String> {
        if module.jdk_home.is_some() {
            return Some(
                "module declares its own jdk_home; this environment compiles against the ambient \
                 JAVA_HOME only, and the bootclasspath decides what `java.*` resolves to"
                    .into(),
            );
        }
        if !module.resources.is_empty() {
            return Some(format!(
                "module declares {} resource director(ies); resources are not copied into the \
                 output, so the module would be short",
                module.resources.len()
            ));
        }
        if module.source_roots.iter().any(|root| root.generated) {
            return Some(
                "module declares generated source roots; this environment has no generation step"
                    .into(),
            );
        }
        if !module.processor_path.is_empty() {
            return Some(format!(
                "module declares {} annotation/symbol processor(s); they are not run, so their \
                 generated sources would be missing from the output",
                module.processor_path.len()
            ));
        }
        if !module.processor_options.is_empty() {
            return Some(format!(
                "module declares {} annotation/symbol processor option(s), but no processor \
                 execution contract is implemented",
                module.processor_options.len()
            ));
        }
        if module.outputs.is_empty() {
            return Some("module declares no output to materialize".into());
        }
        if module.outputs.len() != 1 {
            return Some(format!(
                "module declares {} outputs; one compiler invocation produces one artifact tree, \
                 so copying it to multiple outputs is not a supported output contract",
                module.outputs.len()
            ));
        }
        if let Some(argument) = unsupported_opaque_argument(&module.kotlinc_args) {
            return Some(format!(
                "module compiler argument {argument:?} can introduce an unmodelled input; record \
                 that input in the module model instead"
            ));
        }
        if let Some(jar) = module
            .outputs
            .iter()
            .find(|output| matches!(output, crate::model::ModuleOutput::Jar(_)))
        {
            return Some(format!(
                "module requests a jar output ({}); jar packaging is not implemented",
                jar.path().display()
            ));
        }
        None
    }

    fn base_inputs(
        &self,
        module: &Module,
        dependency_outputs: &[PathBuf],
    ) -> Result<CacheKeyInputs, String> {
        if let Some(argument) = unsupported_opaque_argument(&self.common_args) {
            return Err(format!(
                "common compiler argument {argument:?} can introduce an unmodelled input"
            ));
        }
        let mut sources = Vec::new();
        for path in Self::sources_of(module)? {
            sources.push(
                FileDigest::of_file(&path)
                    .map_err(|error| format!("cannot read source {}: {error}", path.display()))?,
            );
        }

        // Classpath entries are digested by BOTH path and content: a jar's file name is load-bearing
        // (kotlinx.serialization picks its `write$Self` mangling by parsing it), so a rename changes
        // compilation even when the bytes do not. A directory has no single content hash, so its
        // digest covers the files beneath it.
        let keyed_classpath = self.keyed_classpath(module);
        let compiler_classpath = self.compile_classpath(module, dependency_outputs);
        let implicit = JvmCompilationInputInventory::from_effective_classpath(&compiler_classpath);
        let mut classpath = Vec::new();
        for entry in &keyed_classpath {
            classpath.push(digest_path(entry)?);
        }
        if let Some(entry) = implicit.common_expectation_klib() {
            classpath.push(digest_path(entry)?);
        }
        let mut friend_paths = Vec::new();
        for entry in &module.friend_paths {
            friend_paths.push(digest_path(entry)?);
        }

        let mut flags: Vec<String> = Vec::new();
        if let Some(name) = &module.module_name {
            flags.push("-module-name".into());
            flags.push(name.clone());
        }
        if let Some(target) = &module.jvm_target {
            flags.push("-jvm-target".into());
            flags.push(target.clone());
        }
        flags.extend(module.kotlinc_args.iter().cloned());
        flags.extend(self.common_args.iter().cloned());
        flags.extend(HERMETIC_CLASSPATH_ARGS.iter().map(|arg| (*arg).into()));

        let mut plugins = Vec::new();
        for jar in &module.processor_path {
            plugins.push(digest_path(jar)?);
        }

        Ok(CacheKeyInputs {
            compiler: self.compiler_identity.clone()?,
            compiler_flags: flags,
            environment: CacheKeyInputs::environment_from_process(),
            jdk_identity: self.jdk_identity.clone()?,
            sources,
            classpath,
            friend_paths,
            dependency_abis: Vec::new(), // the driver fills this in
            plugins,
            plugin_options: module.processor_options.clone(),
            target: module
                .jvm_target
                .clone()
                .unwrap_or_else(|| "jvm-default".into()),
        })
    }

    fn compile(
        &mut self,
        module: &Module,
        dependency_outputs: &[PathBuf],
    ) -> Result<CompiledModule, String> {
        let output = self
            .scratch
            .join(format!("compile-{}", module_scratch_component(module)));
        let _ = std::fs::remove_dir_all(&output);
        std::fs::create_dir_all(&output)
            .map_err(|error| format!("cannot create {}: {error}", output.display()))?;

        let sources = Self::sources_of(module)?;
        let args = self.module_args(module, &sources, &output, dependency_outputs);
        let result = Command::new(&self.binary)
            .args(&args)
            .output()
            .map_err(|error| format!("cannot run {}: {error}", self.binary.display()))?;

        if !result.status.success() {
            // The compiler's own diagnostics are the useful part; the exit code alone is not.
            return Err(format!(
                "krusty exited with {}: {}{}",
                result.status,
                String::from_utf8_lossy(&result.stderr).trim(),
                String::from_utf8_lossy(&result.stdout).trim(),
            ));
        }

        let mut artifacts = Vec::new();
        collect_artifacts(&output, &output, &mut artifacts)?;
        // Sorted so the artifact list is stable regardless of directory-read order, which is not
        // guaranteed by the filesystem.
        artifacts.sort_by(|a, b| a.0.cmp(&b.0));
        if artifacts.is_empty() {
            return Err("krusty exited successfully but emitted no artifacts".into());
        }
        Ok(CompiledModule {
            artifacts,
            // The reduced class-header model does not yet carry every non-SOURCE annotation or
            // inline body. Publishing it as complete would let a dependent reuse stale output.
            // The generic driver still supports exact reduced ABIs from environments that can
            // prove completeness; this concrete adapter remains sound by publishing whole output.
            published_abi: PublishedAbi::WholeOutput {
                reason: "the JVM reduced ABI does not yet carry every annotation and inline body"
                    .into(),
            },
        })
    }
}

/// A module id is opaque build-tool data, not a path. Hash its exact bytes into one fixed-width
/// component rather than trying to maintain a platform-specific separator/blocklist.
fn module_scratch_component(module: &Module) -> String {
    module.id.as_ref().map_or_else(
        || "anonymous".into(),
        |id| {
            format!(
                "module-{}",
                crate::digest::digest_bytes(id.as_str().as_bytes())
            )
        },
    )
}

/// Digest a classpath entry. A file hashes its bytes; a directory hashes the sorted
/// `(relative path, content)` of everything beneath it, so a dependency's output directory has a
/// stable identity even though it is not one file.
fn digest_path(path: &Path) -> Result<FileDigest, String> {
    if path.is_dir() {
        let mut entries = Vec::new();
        collect_artifacts(path, path, &mut entries)
            .map_err(|error| format!("cannot digest directory {}: {error}", path.display()))?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        // Length-delimited, like every other identity in this crate: a file NAME containing a
        // newline must not be able to impersonate a second entry.
        let mut hasher = crate::digest::Hasher::new();
        hasher.count("entries", entries.len());
        for (name, bytes) in &entries {
            hasher.text("name", name);
            hasher.nested("content", crate::digest::digest_bytes(bytes));
        }
        return Ok(FileDigest::new(path, hasher.finish()));
    }
    match FileDigest::of_file(path) {
        Ok(digest) => Ok(digest),
        // A classpath entry that does not exist yet is still part of the key: its absence is an
        // input, and it must not silently hash the same as a present one.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(FileDigest::of_bytes(path, ABSENT_ENTRY_MARKER))
        }
        Err(error) => Err(format!("cannot digest {}: {error}", path.display())),
    }
}

/// Distinguishes "this classpath entry does not exist" from any real file content.
const ABSENT_ENTRY_MARKER: &[u8] = b"krusty-build:absent-classpath-entry";

fn collect_kotlin(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("cannot read source root {}: {error}", root.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("cannot read an entry under {}: {error}", root.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_kotlin(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "kt") {
            out.push(path);
        }
    }
    Ok(())
}

/// Raw CLI arguments may carry sources or path-bearing compiler inputs that have no corresponding
/// field in [`Module`]. Scalar switches are already part of the key and are safe; opaque argument
/// files, positional arguments, and the path-bearing options understood by the current CLI are
/// refused until their referenced bytes have an owned model.
fn unsupported_opaque_argument(arguments: &[String]) -> Option<&str> {
    arguments.iter().map(String::as_str).find(|argument| {
        argument.starts_with('@')
            || !argument.starts_with('-')
            || matches!(
                *argument,
                "-d" | "-cp"
                    | "-classpath"
                    | "-class-path"
                    | "-jdk-home"
                    | "-module-name"
                    | "-jvm-target"
            )
            || argument.starts_with("-Xfriend-paths=")
            || argument.starts_with("-Xplugin=")
            || argument.starts_with("-module-name=")
            || argument.starts_with("-jvm-target=")
            || argument.starts_with("-jdk-home=")
            || argument.starts_with("-classpath=")
            || argument.starts_with("-class-path=")
            || argument.starts_with("-cp=")
            || argument.starts_with("-d=")
    })
}

/// Read every file under `directory` as `(path relative to `base`, bytes)`.
fn collect_artifacts(
    base: &Path,
    directory: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_artifacts(base, &path, out)?;
            continue;
        }
        let relative = path
            .strip_prefix(base)
            .map_err(|_| format!("{} escaped {}", path.display(), base.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        out.push((relative, bytes));
    }
    Ok(())
}

fn join_classpath(entries: &[PathBuf]) -> String {
    let separator = if cfg!(windows) { ";" } else { ":" };
    entries
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(separator)
}

/// Identify the compiler binary by its own version string plus its content hash. The content hash
/// is what a version string cannot give you: a rebuilt compiler at the same version must not reuse
/// the previous build's artifacts.
fn identify_compiler(binary: &Path) -> Result<String, String> {
    let version = Command::new(binary)
        .arg("-version")
        .output()
        .ok()
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            let text = if text.trim().is_empty() {
                String::from_utf8_lossy(&out.stderr).to_string()
            } else {
                text.to_string()
            };
            text.lines().next().unwrap_or("").trim().to_string()
        })
        .unwrap_or_default();
    let content = std::fs::read(binary)
        .map(|bytes| crate::digest::digest_bytes(&bytes).to_string())
        .map_err(|error| format!("cannot identify compiler {}: {error}", binary.display()))?;
    Ok(format!("{version} [{content}]"))
}

/// Identify the exact JDK image the compiler reads, not merely its release label. Patched JDKs can
/// share a `release` file while exposing different `java.*` declarations.
fn identify_jdk(modules: Option<&Path>) -> Result<String, String> {
    let Some(modules) = modules else {
        return Ok("jdk:none".into());
    };
    let content = FileDigest::of_file(modules)
        .map_err(|error| format!("cannot identify JDK image {}: {error}", modules.display()))?;
    Ok(format!("jdk:{}:{}", modules.display(), content.content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModuleId, SourceRoot, SourceRootKind};

    fn module_with_sources(root: &Path) -> Module {
        let mut module = Module::new(ModuleId::new("demo"), root);
        module.source_roots = vec![SourceRoot {
            path: root.to_path_buf(),
            kind: SourceRootKind::Main,
            generated: false,
        }];
        module
    }

    #[test]
    fn sources_are_collected_recursively_and_sorted() {
        let root = std::env::temp_dir().join(format!("krusty-build-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nested")).expect("mkdir");
        std::fs::write(root.join("Zeta.kt"), "fun z() {}").expect("write");
        std::fs::write(root.join("Alpha.kt"), "fun a() {}").expect("write");
        std::fs::write(root.join("nested/Beta.kt"), "fun b() {}").expect("write");
        std::fs::write(root.join("notes.txt"), "ignored").expect("write");

        let sources = KrustyCli::sources_of(&module_with_sources(&root)).expect("readable roots");
        let names: Vec<String> = sources
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["Alpha.kt", "Zeta.kt", "Beta.kt"],
            "sorted by full path, and only .kt files"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn opaque_module_ids_become_one_digest_named_scratch_component() {
        let id = r"..\..\victim/../../outside:module";
        let module = Module::new(ModuleId::new(id), "/repo");
        let component = module_scratch_component(&module);
        assert_eq!(
            component,
            format!("module-{}", crate::digest::digest_bytes(id.as_bytes())),
            "the complete opaque identity, rather than selected separators, names the directory"
        );
        assert_eq!(
            Path::new(&component).components().count(),
            1,
            "the encoded identity cannot introduce a parent or platform separator component"
        );
        let scratch = Path::new("/scratch");
        assert!(scratch
            .join(format!("compile-{component}"))
            .starts_with(scratch));
    }

    #[test]
    fn an_absent_classpath_entry_still_has_a_stable_distinct_digest() {
        let missing = Path::new("/nonexistent/krusty-build/absent.jar");
        let digest = digest_path(missing).expect("absence is not an error");
        assert_eq!(digest.path, missing);
        assert_eq!(
            digest.content,
            crate::digest::digest_bytes(ABSENT_ENTRY_MARKER),
            "an absent entry is an input, not a hole in the key"
        );
    }

    #[test]
    fn a_directory_digest_changes_when_a_file_beneath_it_changes() {
        let root = std::env::temp_dir().join(format!("krusty-build-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("pkg")).expect("mkdir");
        std::fs::write(root.join("pkg/A.class"), b"one").expect("write");
        let before = digest_path(&root).expect("digest");

        std::fs::write(root.join("pkg/A.class"), b"two").expect("write");
        let after = digest_path(&root).expect("digest");
        assert_ne!(
            before.content, after.content,
            "a dependency output directory must have content identity, not just a path"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding: a `read_dir` failure silently produced a shorter source list, which compiled to a
    /// partial module and cached it under a key that looked valid.
    #[test]
    fn an_unreadable_source_root_is_an_error_not_a_short_source_list() {
        let mut module = Module::new(ModuleId::new("demo"), "/nonexistent");
        module.source_roots = vec![SourceRoot {
            path: PathBuf::from("/nonexistent/krusty-build/source-root"),
            kind: SourceRootKind::Main,
            generated: false,
        }];
        let error = KrustyCli::sources_of(&module).expect_err("must not silently return empty");
        assert!(
            error.contains("cannot read source root"),
            "the error must name the root: {error}"
        );
    }

    #[test]
    fn friend_paths_are_both_classpath_entries_and_exact_friend_arguments() {
        let cli = KrustyCli {
            binary: PathBuf::from("/compiler"),
            compiler_identity: Ok("compiler".into()),
            jdk_identity: Ok("jdk".into()),
            jdk_modules: None,
            common_args: Vec::new(),
            scratch: PathBuf::from("/scratch"),
        };
        let mut module = Module::new(ModuleId::new("test"), "/repo");
        module.classpath.push(PathBuf::from("/cp.jar"));
        module.friend_paths.push(PathBuf::from("/out/main"));
        module.friend_paths.push(PathBuf::from("/out/generated"));
        let args = cli.module_args(&module, &[], Path::new("/out/test"), &[]);

        let friends = vec![PathBuf::from("/out/main"), PathBuf::from("/out/generated")];
        assert_eq!(
            args,
            vec![
                "-cp".to_string(),
                join_classpath(&[
                    PathBuf::from("/cp.jar"),
                    PathBuf::from("/out/main"),
                    PathBuf::from("/out/generated"),
                ]),
                format!("-Xfriend-paths={}", join_classpath(&friends)),
                "-no-stdlib".to_string(),
                "-no-reflect".to_string(),
                "-no-jdk".to_string(),
                "-d".to_string(),
                "/out/test".to_string(),
            ],
            "the compiler argument vector must carry the exact friend-path set and hermetic flags"
        );
    }

    #[test]
    fn opaque_path_bearing_arguments_are_refused() {
        for argument in [
            "@compiler.args",
            "/extra/Source.kt",
            "-cp",
            "-jdk-home",
            "-Xfriend-paths=/out/main",
            "-Xplugin=/plugins/generator.jar",
            "-module-name=shadowed",
            "-jvm-target=21",
            "-cp=/unkeyed.jar",
        ] {
            assert_eq!(
                unsupported_opaque_argument(&[argument.into()]),
                Some(argument),
                "{argument} introduces bytes outside the module model"
            );
        }
        assert_eq!(
            unsupported_opaque_argument(&["-Xno-param-assertions".into()]),
            None,
            "a scalar output switch is itself completely represented in the key"
        );
    }

    #[test]
    fn model_owned_option_tokens_are_keyed_exactly_and_raw_aliases_are_refused() {
        let cli = KrustyCli {
            binary: PathBuf::from("/compiler"),
            compiler_identity: Ok("compiler".into()),
            jdk_identity: Ok("jdk".into()),
            jdk_modules: None,
            common_args: Vec::new(),
            scratch: PathBuf::from("/scratch"),
        };
        let mut module = Module::new(ModuleId::new("demo"), "/repo");
        module.module_name = Some("named".into());
        module.jvm_target = Some("21".into());
        let inputs = cli.base_inputs(&module, &[]).expect("inputs");
        let args = cli.module_args(&module, &[], Path::new("/out"), &[]);
        assert_eq!(
            &inputs.compiler_flags[..4],
            ["-module-name", "named", "-jvm-target", "21"],
            "the key uses the same tokenization as the compiler invocation"
        );
        assert_eq!(
            &args[..4],
            &inputs.compiler_flags[..4],
            "the owned option tokens in the key and actual invocation must be identical"
        );

        let mut alias = Module::new(ModuleId::new("alias"), "/repo");
        alias.kotlinc_args = vec!["-module-name=named".into()];
        assert!(cli.unsupported(&alias).is_some());

        let mut one_token = inputs.clone();
        one_token.compiler_flags = vec!["-module-name=named".into()];
        assert_ne!(
            inputs.key(),
            one_token.key(),
            "one raw alias token cannot collide with the owned two-token invocation"
        );
    }

    #[test]
    fn generated_roots_and_multiple_output_trees_are_refused_explicitly() {
        let cli = KrustyCli {
            binary: PathBuf::from("/compiler"),
            compiler_identity: Ok("compiler".into()),
            jdk_identity: Ok("jdk".into()),
            jdk_modules: None,
            common_args: Vec::new(),
            scratch: PathBuf::from("/scratch"),
        };
        let mut generated = Module::new(ModuleId::new("generated"), "/repo");
        generated.source_roots.push(SourceRoot {
            path: PathBuf::from("/repo/generated"),
            kind: SourceRootKind::Main,
            generated: true,
        });
        assert_eq!(
            cli.unsupported(&generated).as_deref(),
            Some("module declares generated source roots; this environment has no generation step")
        );

        let mut multiple = Module::new(ModuleId::new("multiple"), "/repo");
        multiple.outputs = vec![
            crate::model::ModuleOutput::ClassDirectory("/out/one".into()),
            crate::model::ModuleOutput::ClassDirectory("/out/two".into()),
        ];
        assert!(
            cli.unsupported(&multiple)
                .is_some_and(|reason| reason.contains("one artifact tree")),
            "multi-output mirroring must be refused rather than silently copying one tree"
        );
    }

    #[test]
    fn the_implicit_common_klib_is_a_keyed_classpath_input() {
        let root = std::env::temp_dir().join(format!(
            "krusty-build-implicit-cp-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("mkdir");
        let stdlib = root.join("kotlin-stdlib.jar");
        let common = root.join("kotlin-stdlib-wasm-js.klib");
        std::fs::write(&stdlib, b"stdlib").expect("stdlib");
        std::fs::write(&common, b"common metadata").expect("common klib");

        let cli = KrustyCli {
            binary: PathBuf::from("/compiler"),
            compiler_identity: Ok("compiler".into()),
            jdk_identity: Ok("jdk".into()),
            jdk_modules: None,
            common_args: Vec::new(),
            scratch: root.join("scratch"),
        };
        let mut module = Module::new(ModuleId::new("demo"), &root);
        module.classpath.push(stdlib.clone());
        let inputs = cli.base_inputs(&module, &[]).expect("inputs");
        assert_eq!(
            inputs
                .classpath
                .iter()
                .map(|entry| entry.path.as_path())
                .collect::<Vec<_>>(),
            vec![stdlib.as_path(), common.as_path()]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_dependency_classpath_companion_is_inventoried_before_lookup() {
        let root = std::env::temp_dir().join(format!(
            "krusty-build-dependency-companion-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("mkdir");
        let dependency = root.join("kotlin-stdlib.jar");
        let common = root.join("kotlin-stdlib-wasm-js.klib");
        std::fs::write(&dependency, b"dependency archive").expect("dependency");
        std::fs::write(&common, b"common metadata").expect("common");

        let cli = KrustyCli {
            binary: PathBuf::from("/compiler"),
            compiler_identity: Ok("compiler".into()),
            jdk_identity: Ok("jdk".into()),
            jdk_modules: None,
            common_args: Vec::new(),
            scratch: root.join("scratch"),
        };
        let module = Module::new(ModuleId::new("consumer"), &root);
        let inputs = cli
            .base_inputs(&module, std::slice::from_ref(&dependency))
            .expect("inputs");
        assert_eq!(
            inputs
                .classpath
                .iter()
                .map(|entry| entry.path.as_path())
                .collect::<Vec<_>>(),
            vec![common.as_path()],
            "the sidecar selected by the actual full compiler classpath is keyed even when its \
             anchor is a dependency output"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn jdk_identity_hashes_the_modules_image_not_a_release_label() {
        let path =
            std::env::temp_dir().join(format!("krusty-build-jdk-modules-{}", std::process::id()));
        std::fs::write(&path, b"image one").expect("first image");
        let before = identify_jdk(Some(&path)).expect("identity");
        std::fs::write(&path, b"image two").expect("second image");
        let after = identify_jdk(Some(&path)).expect("identity");
        assert_ne!(before, after);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn classpath_joins_with_the_platform_separator() {
        let joined = join_classpath(&[PathBuf::from("/a.jar"), PathBuf::from("/b")]);
        assert!(joined.contains("a.jar"));
        assert!(joined.contains('b'));
        assert_eq!(
            joined
                .matches(if cfg!(windows) { ';' } else { ':' })
                .count(),
            1
        );
    }
}
