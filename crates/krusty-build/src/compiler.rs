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

use crate::cache::{CacheKeyInputs, FileDigest};
use crate::driver::{BuildEnvironment, CompiledModule};
use crate::model::Module;

/// Drives `krusty` as a subprocess.
#[derive(Clone, Debug)]
pub struct KrustyCli {
    binary: PathBuf,
    /// Compiler identity for the cache key — ideally version plus build id, so a rebuilt compiler
    /// with an unchanged version string cannot reuse the old one's artifacts.
    compiler_identity: String,
    /// JDK identity for the cache key: the bootclasspath decides what `java.*` resolves to, so two
    /// JDKs compile the same sources differently.
    jdk_identity: String,
    /// Flags passed to every module, after the module's own `kotlinc_args`.
    common_args: Vec<String>,
    /// Scratch directory for per-module output before it is read back.
    scratch: PathBuf,
}

impl KrustyCli {
    pub fn new(binary: impl Into<PathBuf>, scratch: impl Into<PathBuf>) -> Self {
        let binary = binary.into();
        Self {
            compiler_identity: identify_compiler(&binary),
            jdk_identity: identify_jdk(),
            binary,
            common_args: vec!["-no-reflect".into()],
            scratch: scratch.into(),
        }
    }

    /// Override the compiler identity string that enters every cache key.
    pub fn with_compiler_identity(mut self, identity: impl Into<String>) -> Self {
        self.compiler_identity = identity.into();
        self
    }

    pub fn with_common_args(mut self, args: Vec<String>) -> Self {
        self.common_args = args;
        self
    }

    /// A module's Kotlin sources, sorted by path — the canonical compile order. See module docs.
    pub fn sources_of(module: &Module) -> Vec<PathBuf> {
        let mut sources = Vec::new();
        for root in &module.source_roots {
            collect_kotlin(&root.path, &mut sources);
        }
        sources.sort();
        sources
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
        entries
    }

    fn module_args(
        &self,
        module: &Module,
        output: &Path,
        dependency_outputs: &[PathBuf],
    ) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        for source in Self::sources_of(module) {
            args.push(source.display().to_string());
        }
        let classpath = self.compile_classpath(module, dependency_outputs);
        if !classpath.is_empty() {
            args.push("-cp".into());
            args.push(join_classpath(&classpath));
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
        args.push("-d".into());
        args.push(output.display().to_string());
        args
    }
}

impl BuildEnvironment for KrustyCli {
    fn base_inputs(&self, module: &Module) -> Result<CacheKeyInputs, String> {
        let mut sources = Vec::new();
        for path in Self::sources_of(module) {
            sources.push(
                FileDigest::of_file(&path)
                    .map_err(|error| format!("cannot read source {}: {error}", path.display()))?,
            );
        }

        // Classpath entries are digested by BOTH path and content: a jar's file name is load-bearing
        // (kotlinx.serialization picks its `write$Self` mangling by parsing it), so a rename changes
        // compilation even when the bytes do not. A directory has no single content hash, so its
        // digest covers the files beneath it.
        let mut classpath = Vec::new();
        for entry in self.keyed_classpath(module) {
            classpath.push(digest_path(&entry)?);
        }
        let mut friend_paths = Vec::new();
        for entry in &module.friend_paths {
            friend_paths.push(digest_path(entry)?);
        }

        let mut flags: Vec<String> = Vec::new();
        if let Some(name) = &module.module_name {
            flags.push(format!("-module-name={name}"));
        }
        if let Some(target) = &module.jvm_target {
            flags.push(format!("-jvm-target={target}"));
        }
        flags.extend(module.kotlinc_args.iter().cloned());
        flags.extend(self.common_args.iter().cloned());

        let mut plugins = Vec::new();
        for jar in &module.processor_path {
            plugins.push(digest_path(jar)?);
        }

        Ok(CacheKeyInputs {
            compiler: self.compiler_identity.clone(),
            compiler_flags: flags,
            environment: CacheKeyInputs::environment_from_process(),
            jdk_identity: self.jdk_identity.clone(),
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
        let id = module
            .id
            .as_ref()
            .map(|id| id.as_str().replace(['/', ':'], "_"))
            .unwrap_or_else(|| "anonymous".into());
        let output = self.scratch.join(format!("compile-{id}"));
        let _ = std::fs::remove_dir_all(&output);
        std::fs::create_dir_all(&output)
            .map_err(|error| format!("cannot create {}: {error}", output.display()))?;

        let args = self.module_args(module, &output, dependency_outputs);
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
        Ok(CompiledModule { artifacts })
    }
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
        let mut rendered = String::new();
        for (name, bytes) in &entries {
            rendered.push_str(&format!("{name}:{:016x}\n", crate::fnv1a(bytes)));
        }
        return Ok(FileDigest::new(path, crate::fnv1a(rendered.as_bytes())));
    }
    match FileDigest::of_file(path) {
        Ok(digest) => Ok(digest),
        // A classpath entry that does not exist yet is still part of the key: its absence is an
        // input, and it must not silently hash the same as a present one.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(FileDigest::new(path, crate::fnv1a(b"<absent>")))
        }
        Err(error) => Err(format!("cannot digest {}: {error}", path.display())),
    }
}

fn collect_kotlin(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_kotlin(&path, out);
        } else if path.extension().is_some_and(|e| e == "kt") {
            out.push(path);
        }
    }
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
fn identify_compiler(binary: &Path) -> String {
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
        .map(|bytes| crate::fnv1a(&bytes))
        .unwrap_or(0);
    format!("{version} [{content:016x}]")
}

/// Identify the JDK by `JAVA_HOME`'s `release` file when present, falling back to the path.
fn identify_jdk() -> String {
    let Ok(home) = std::env::var("JAVA_HOME") else {
        return "jdk:unknown".into();
    };
    let release = Path::new(&home).join("release");
    match std::fs::read(&release) {
        Ok(bytes) => format!("jdk:{:016x}", crate::fnv1a(&bytes)),
        Err(_) => format!("jdk-path:{home}"),
    }
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

        let sources = KrustyCli::sources_of(&module_with_sources(&root));
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
    fn an_absent_classpath_entry_still_has_a_stable_distinct_digest() {
        let missing = Path::new("/nonexistent/krusty-build/absent.jar");
        let digest = digest_path(missing).expect("absence is not an error");
        assert_eq!(digest.path, missing);
        assert_eq!(
            digest.content,
            crate::fnv1a(b"<absent>"),
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
