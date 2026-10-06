//! Reference `.class` collection for the Kotlin box corpus.
//!
//! The byte-equality conformance metric measures krusty's emitted classes against the pinned
//! reference `kotlinc`'s for every applicable box case. This module compiles a case with the
//! reference compiler, mirroring the exact topology krusty compiled it under — a single file, a
//! `// FILE:` multi-file set, mixed Kotlin/Java, and a `// MODULE:` multi-module build — and
//! returns the resulting artifacts keyed by `(module identity, JVM internal class name)`.
//!
//! Only `.class` artifacts enter the inventory. `kotlinc`'s `META-INF/<module>.kotlin_module` and
//! any other non-class output are not part of the compared population. A class name that repeats
//! across two modules stays two distinct artifacts: the module identity, not the bare internal
//! name, is the key.
//!
//! A reference failure — a compiler rejection, a missing input, an unreadable class — is returned
//! explicitly as `Err`. No applicable case is dropped from the population by its shape.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use krusty::conformance::{
    directive, inject_support_module, module_units, split_files, split_modules, BoxJdk,
};

use super::common::{self, byte_dump, language_directives};

/// The stable module identity for every single-module topology (ordinary, `// FILE:` multi-file,
/// and mixed Java). It matches the module name krusty emits those cases under.
pub const MAIN_MODULE: &str = "main";

/// Bump when the on-disk cache layout or the comparison population changes — invalidates every
/// cached entry. `v2` added per-module nesting and Java `.class` artifacts to `v1`'s flat tree.
const REF_CACHE_SALT: &str = "ref-classes-v2-modular";

/// A reference `.class` inventory grouped by module identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReferenceClasses {
    modules: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
}

impl ReferenceClasses {
    fn insert(&mut self, module: &str, name: &str, bytes: Vec<u8>) {
        self.modules
            .entry(module.to_string())
            .or_default()
            .insert(name.to_string(), bytes);
    }

    /// Every artifact as `((module, internal name), bytes)`, module- then name-sorted.
    pub fn qualified(&self) -> impl Iterator<Item = ((&str, &str), &[u8])> {
        self.modules.iter().flat_map(|(module, classes)| {
            classes
                .iter()
                .map(move |(name, bytes)| ((module.as_str(), name.as_str()), bytes.as_slice()))
        })
    }

    /// The classes a single module emitted, keyed by internal name.
    pub fn module(&self, name: &str) -> Option<&BTreeMap<String, Vec<u8>>> {
        self.modules.get(name)
    }

    /// Total number of `.class` artifacts across all modules.
    pub fn class_count(&self) -> usize {
        self.modules.values().map(BTreeMap::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.class_count() == 0
    }

    /// Collapse to internal-name → bytes, discarding module identity (first module wins on a
    /// collision). For the opt-in byte-diff inspection only; byte scoring pairs on the full
    /// module-qualified identity so distinct-module same-name classes are never merged.
    pub fn flatten(&self) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        for classes in self.modules.values() {
            for (name, bytes) in classes {
                out.entry(name.clone()).or_insert_with(|| bytes.clone());
            }
        }
        out
    }
}

/// Compile `src` with the reference `kotlinc` into a module-qualified `.class` inventory, mirroring
/// the topology krusty compiled it under. `coroutine_helpers` is the same generated support source
/// the production harness injects for `// WITH_COROUTINES` cases; `cp_jars` and `jdk` are the exact
/// directive-selected classpath and JDK krusty used.
///
/// Results — success and a deterministic, source-anchored compile rejection — are cached on disk
/// keyed by every input (source, stem, helpers, classpath/JDK arguments, and the exact compiler
/// identity), so a re-run pays only for cases whose inputs changed and never replays one exact
/// version's bytes for another. A transient failure (driver crash, work-dir clobber) and a
/// snapshot/dev/beta compiler are not cached.
pub fn reference_compile(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk: BoxJdk<'_>,
    coroutine_helpers: &str,
) -> Result<ReferenceClasses, String> {
    let base_args = jdk.kotlinc_args(cp_jars)?;
    let language_args = language_directives::kotlinc_args(src);
    let (compiler_id, compiler_len) = compiler_identity();
    let fingerprint = reference_cache_fingerprint(
        src,
        stem,
        coroutine_helpers,
        &base_args,
        &language_args,
        compiler_id.as_deref(),
        compiler_len,
    );
    let cache = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("target/cache/ref-classes/{fingerprint:032x}"));
    // A snapshot/dev/beta compiler is not an immutable build, so its output is never cached.
    let cacheable = byte_dump::published_compiler_id().is_some();
    if cacheable {
        if let Some(cached) = load_cache(&cache) {
            return cached;
        }
    }

    let scratch = ScratchDir::new()?;
    let result = compile_all(
        src,
        stem,
        cp_jars,
        jdk,
        coroutine_helpers,
        &language_args,
        scratch.path(),
    );
    if cacheable {
        store_cache(&cache, fingerprint, &result);
    }
    result
}

/// Read every `.class` under `dir` into an internal-name → bytes map (package `/` separators, no
/// extension). `Err` on any read failure — a concurrently rewritten cache must re-grade as a
/// reference failure, not compare against truncated bytes.
pub fn read_class_tree(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut out = BTreeMap::new();
    read_class_tree_in(dir, dir, &mut out)?;
    Ok(out)
}

fn read_class_tree_in(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|error| format!("read dir {}: {error}", dir.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("read dir entry in {}: {error}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            read_class_tree_in(root, &path, out)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "class")
        {
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .with_extension("")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            let bytes =
                std::fs::read(&path).map_err(|error| format!("cache read {relative}: {error}"))?;
            out.insert(relative, bytes);
        }
    }
    Ok(())
}

/// The pure cache key over every input that can change the reference bytes. Kept free of I/O so
/// version isolation (a different compiler identity yields a different key) is directly testable.
pub fn reference_cache_fingerprint(
    src: &str,
    stem: &str,
    coroutine_helpers: &str,
    base_args: &[String],
    language_args: &[String],
    compiler_id: Option<&str>,
    compiler_len: u64,
) -> u128 {
    let mut parts: Vec<Vec<u8>> = vec![
        REF_CACHE_SALT.as_bytes().to_vec(),
        src.as_bytes().to_vec(),
        stem.as_bytes().to_vec(),
        coroutine_helpers.as_bytes().to_vec(),
    ];
    for arg in base_args.iter().chain(language_args) {
        parts.push(arg.as_bytes().to_vec());
    }
    // The exact compiler identity isolates one release/RC's bytes from another's; the jar length
    // guards a same-identity rebuild. An unpublished compiler never reaches the cache, but still
    // contributes a stable key so an in-run second lookup matches.
    parts.push(compiler_id.unwrap_or("").as_bytes().to_vec());
    parts.push(compiler_len.to_le_bytes().to_vec());
    let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    byte_dump::fingerprint_parts(&refs)
}

fn compiler_identity() -> (Option<String>, u64) {
    match common::kotlin_compiler_jar() {
        Some(jar) => {
            let len = std::fs::metadata(&jar).map(|meta| meta.len()).unwrap_or(0);
            (Some(jar.to_string_lossy().into_owned()), len)
        }
        None => (None, 0),
    }
}

fn compile_all(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk: BoxJdk<'_>,
    coroutine_helpers: &str,
    language_args: &[String],
    work: &Path,
) -> Result<ReferenceClasses, String> {
    let mut classes = ReferenceClasses::default();
    if src.contains("// MODULE:") {
        let Some(mut modules) = split_modules(src) else {
            return Err("reference oracle does not support this // MODULE: shape".to_string());
        };
        if directive(src, "WITH_COROUTINES") {
            inject_support_module(&mut modules, coroutine_helpers);
        }
        let mut outputs: HashMap<String, PathBuf> = HashMap::new();
        for unit in module_units(&modules) {
            let mut classpath = cp_jars.to_vec();
            let mut friends = Vec::new();
            for dependency in &unit.deps {
                let Some(output) = outputs.get(dependency) else {
                    return Err(format!(
                        "reference module {}: dependency {dependency} was not built",
                        unit.name
                    ));
                };
                classpath.push(output.clone());
                if unit.friends.contains(dependency) {
                    friends.push(output.clone());
                }
            }
            let output = work.join(&unit.name).join("classes");
            let unit_classes = compile_unit(
                &Unit {
                    module: &unit.name,
                    kotlin: &unit.files,
                    java: &unit.java_files,
                    common_file_count: unit.common_file_count,
                    classpath: &classpath,
                    friend_paths: &friends,
                },
                jdk,
                language_args,
                &work.join(&unit.name),
                &output,
            )?;
            for (name, bytes) in unit_classes {
                classes.insert(&unit.name, &name, bytes);
            }
            outputs.insert(unit.name.clone(), output);
        }
    } else {
        let (mut kotlin, java) = if src.contains("// FILE:") {
            split_files(src)
        } else {
            (vec![(stem.to_string(), src.to_string())], Vec::new())
        };
        if directive(src, "WITH_COROUTINES") {
            kotlin.push(("CoroutineUtil".to_string(), coroutine_helpers.to_string()));
        }
        let output = work.join(MAIN_MODULE).join("classes");
        let unit_classes = compile_unit(
            &Unit {
                module: MAIN_MODULE,
                kotlin: &kotlin,
                java: &java,
                common_file_count: 0,
                classpath: cp_jars,
                friend_paths: &[],
            },
            jdk,
            language_args,
            &work.join(MAIN_MODULE),
            &output,
        )?;
        for (name, bytes) in unit_classes {
            classes.insert(MAIN_MODULE, &name, bytes);
        }
    }
    if classes.is_empty() {
        return Err("reference compile produced no .class artifacts".to_string());
    }
    Ok(classes)
}

/// One compilation unit handed to the reference compiler. The first `common_file_count` Kotlin
/// blocks are the folded `dependsOn` (common) sources, in dependency-first order.
struct Unit<'a> {
    module: &'a str,
    kotlin: &'a [(String, String)],
    java: &'a [(String, String)],
    common_file_count: usize,
    classpath: &'a [PathBuf],
    friend_paths: &'a [PathBuf],
}

/// Compile one unit's Kotlin (with its Java sources visible for resolution), then compile its Java
/// with javac against the Kotlin output, exactly as kotlinc's own box-test infrastructure does.
/// Every emitted `.class` — Kotlin and Java — is written under `output` so a dependent module reads
/// it off the classpath, and returned keyed by internal name.
fn compile_unit(
    unit: &Unit<'_>,
    jdk: BoxJdk<'_>,
    language_args: &[String],
    unit_root: &Path,
    output: &Path,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    std::fs::create_dir_all(output)
        .map_err(|error| format!("reference module {}: {error}", unit.module))?;
    if unit.kotlin.is_empty() && unit.java.is_empty() {
        // A source-less hmpp intermediate: no classes, but dependents still need its (empty)
        // classpath directory, which the caller recorded.
        return Ok(BTreeMap::new());
    }

    let source_root = unit_root.join("sources");
    let kotlin_paths = write_group(&source_root.join("kotlin"), unit.kotlin)?;
    let java_paths = write_group(&source_root.join("java"), unit.java)?;

    if !unit.kotlin.is_empty() {
        // Each unit resolves its Kotlin against the base classpath PLUS its already-built
        // dependency modules, so the `-classpath`/`-no-jdk` arguments are derived per unit.
        let classpath_args = jdk.kotlinc_args(unit.classpath)?;
        let mut args: Vec<String> = vec!["-d".to_string(), output.to_string_lossy().into_owned()];
        args.extend(classpath_args);
        args.extend_from_slice(language_args);
        args.push("-module-name".to_string());
        args.push(unit.module.to_string());
        if unit.common_file_count != 0 {
            args.push("-Xmulti-platform".to_string());
            let common = kotlin_paths[..unit.common_file_count]
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(",");
            args.push(format!("-Xcommon-sources={common}"));
        }
        if !unit.friend_paths.is_empty() {
            let friends = std::env::join_paths(unit.friend_paths)
                .map_err(|error| format!("reference module {}: {error}", unit.module))?;
            args.push(format!("-Xfriend-paths={}", friends.to_string_lossy()));
        }
        // Java sources are supplementary input so the Kotlin compile resolves Java declarations;
        // kotlinc emits no `.class` for them (javac does, below).
        for path in kotlin_paths.iter().chain(&java_paths) {
            args.push(path.to_string_lossy().into_owned());
        }
        let (code, diagnostics) =
            common::kotlinc_compile(&args).ok_or_else(|| "kotlinc unavailable".to_string())?;
        if code != 0 {
            return Err(reference_compile_error(unit.module, &diagnostics));
        }
    }

    if !unit.java.is_empty() {
        let mut javac_cp = unit.classpath.to_vec();
        javac_cp.push(output.to_path_buf());
        let (javadir, java_classes) =
            common::javac_compile(unit.java, &javac_cp).ok_or_else(|| {
                format!(
                    "reference module {}: javac rejected the Java sources",
                    unit.module
                )
            })?;
        if let Some(root) = javadir.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
        for (name, bytes) in &java_classes {
            let path = output.join(format!("{name}.class"));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("reference module {}: {error}", unit.module))?;
            }
            std::fs::write(&path, bytes)
                .map_err(|error| format!("reference module {}: {error}", unit.module))?;
        }
    }

    read_class_tree(output)
}

/// Extract the first source-anchored kotlinc diagnostic as the explicit failure cause, falling back
/// to the first `error`/output line so a locationless failure is still reported, never swallowed.
fn reference_compile_error(module: &str, diagnostics: &str) -> String {
    let line = diagnostics
        .lines()
        .find(|line| line.contains(": error:") && line.contains(".kt:"))
        .or_else(|| diagnostics.lines().find(|line| line.contains("error")))
        .or_else(|| diagnostics.lines().next())
        .unwrap_or("reference compiler rejected the sources");
    format!("reference module {module}: {}", line.trim())
}

fn write_group(dir: &Path, blocks: &[(String, String)]) -> Result<Vec<PathBuf>, String> {
    blocks
        .iter()
        .enumerate()
        .map(|(index, (name, source))| {
            let block_dir = dir.join(index.to_string());
            std::fs::create_dir_all(&block_dir)
                .map_err(|error| format!("cannot create reference source directory: {error}"))?;
            // The FILE NAME decides kotlinc's facade class name (`arrayElement.kt` →
            // `ArrayElementKt`) and javac requires it to match the public class, so a Java block's
            // name already carries `.java` while a Kotlin block carries only its leaf stem.
            let leaf = name.rsplit('/').next().unwrap_or(name);
            let file = if name.ends_with(".java") {
                block_dir.join(leaf)
            } else {
                block_dir.join(format!("{leaf}.kt"))
            };
            std::fs::write(&file, source)
                .map_err(|error| format!("cannot write reference source: {error}"))?;
            Ok(file)
        })
        .collect()
}

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Result<ScratchDir, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "krusty_refclasses_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .map_err(|error| format!("cannot create reference scratch directory: {error}"))?;
        Ok(ScratchDir(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Read a cached result. `None` = no cached entry (compile it); `Some(Ok|Err)` = a published
/// success or a cached deterministic rejection.
fn load_cache(cache: &Path) -> Option<Result<ReferenceClasses, String>> {
    if cache.join("FAILED").is_file() {
        let reason = std::fs::read_to_string(cache.join("FAILED")).unwrap_or_default();
        return Some(Err(reason.lines().next().unwrap_or("?").to_string()));
    }
    if cache.join("OK").is_file() {
        return Some(read_cached_classes(cache));
    }
    None
}

fn read_cached_classes(cache: &Path) -> Result<ReferenceClasses, String> {
    let mut classes = ReferenceClasses::default();
    let entries = std::fs::read_dir(cache)
        .map_err(|error| format!("read ref cache {}: {error}", cache.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read ref cache entry: {error}"))?;
        let path = entry.path();
        if !path.is_dir() {
            continue; // the OK / FAILED markers
        }
        let module = entry.file_name().to_string_lossy().into_owned();
        for (name, bytes) in read_class_tree(&path)? {
            classes.insert(&module, &name, bytes);
        }
    }
    Ok(classes)
}

/// Publish a result to the cache. Success and a deterministic source-anchored rejection are stored;
/// a transient failure is left uncached so the next run retries instead of being poisoned.
fn store_cache(cache: &Path, fingerprint: u128, result: &Result<ReferenceClasses, String>) {
    match result {
        Ok(classes) => {
            // Publish atomically: write the whole tree into a unique staging dir, then rename it
            // into place. A concurrent publisher's rename simply loses; a published cache is never
            // deleted out from under a reader.
            let staging = cache.with_file_name(format!(
                "{fingerprint:032x}.stage{}-{}",
                std::process::id(),
                staging_sequence(),
            ));
            let _ = std::fs::remove_dir_all(&staging);
            if write_cache_tree(&staging, classes).is_ok()
                && std::fs::write(staging.join("OK"), b"").is_ok()
            {
                if std::fs::rename(&staging, cache).is_err() {
                    let _ = std::fs::remove_dir_all(&staging);
                }
            } else {
                let _ = std::fs::remove_dir_all(&staging);
            }
        }
        Err(reason) if reason.contains(".kt:") && reason.contains(": error:") => {
            if std::fs::create_dir_all(cache).is_ok() {
                let _ = std::fs::write(cache.join("FAILED"), reason);
            }
        }
        Err(_) => {} // transient / toolchain-unavailable: do not cache
    }
}

fn staging_sequence() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

fn write_cache_tree(staging: &Path, classes: &ReferenceClasses) -> std::io::Result<()> {
    std::fs::create_dir_all(staging)?;
    for ((module, name), bytes) in classes.qualified() {
        let path = staging.join(module).join(format!("{name}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_HELPERS: &str = "";

    fn kotlinc_available() -> bool {
        krusty::toolchain::kotlinc_path().is_some()
    }

    #[test]
    fn cache_key_isolates_version_source_and_classpath() {
        let base = reference_cache_fingerprint(
            "fun box() = \"OK\"",
            "Box",
            NO_HELPERS,
            &["-classpath".to_string(), "/a.jar".to_string()],
            &[],
            Some("2.4.20"),
            100,
        );
        // Same inputs → same key.
        assert_eq!(
            base,
            reference_cache_fingerprint(
                "fun box() = \"OK\"",
                "Box",
                NO_HELPERS,
                &["-classpath".to_string(), "/a.jar".to_string()],
                &[],
                Some("2.4.20"),
                100,
            )
        );
        // A different exact compiler version must not replay the first version's bytes.
        assert_ne!(
            base,
            reference_cache_fingerprint(
                "fun box() = \"OK\"",
                "Box",
                NO_HELPERS,
                &["-classpath".to_string(), "/a.jar".to_string()],
                &[],
                Some("2.4.10"),
                100,
            )
        );
        // A changed source, classpath, or jar length each changes the key.
        assert_ne!(
            base,
            reference_cache_fingerprint(
                "fun box() = \"FAIL\"",
                "Box",
                NO_HELPERS,
                &["-classpath".to_string(), "/a.jar".to_string()],
                &[],
                Some("2.4.20"),
                100,
            )
        );
        assert_ne!(
            base,
            reference_cache_fingerprint(
                "fun box() = \"OK\"",
                "Box",
                NO_HELPERS,
                &["-classpath".to_string(), "/b.jar".to_string()],
                &[],
                Some("2.4.20"),
                100,
            )
        );
        assert_ne!(
            base,
            reference_cache_fingerprint(
                "fun box() = \"OK\"",
                "Box",
                NO_HELPERS,
                &["-classpath".to_string(), "/a.jar".to_string()],
                &[],
                Some("2.4.20"),
                101,
            )
        );
    }

    #[test]
    fn multi_module_keeps_same_named_classes_per_module() {
        if !kotlinc_available() {
            return;
        }
        // Two independent modules each declare `p.Foo`; `main` imports `a`'s. The emitted class
        // `p/Foo` therefore exists in BOTH module `a` and module `b` and must stay two distinct
        // artifacts under their module identities, never flattened to one.
        let src = "// MODULE: a\n\
                   // FILE: a.kt\n\
                   package p\n\
                   class Foo { fun v() = \"A\" }\n\
                   // MODULE: b\n\
                   // FILE: b.kt\n\
                   package p\n\
                   class Foo { fun v() = \"B\" }\n\
                   // MODULE: main(a)\n\
                   // FILE: main.kt\n\
                   import p.Foo\n\
                   fun box(): String = Foo().v()\n";
        let reference = compile_or_skip(src, "typeAliasFixture", &[], BoxJdk::Full { root: None });
        let Some(reference) = reference else {
            return;
        };
        let a = reference.module("a").expect("module a must emit its class");
        let b = reference.module("b").expect("module b must emit its class");
        assert!(a.contains_key("p/Foo"), "module a classes: {:?}", a.keys());
        assert!(b.contains_key("p/Foo"), "module b classes: {:?}", b.keys());
        assert_ne!(
            a.get("p/Foo"),
            b.get("p/Foo"),
            "the two modules' p/Foo differ in bytes (one returns A, the other B)"
        );
        assert!(
            reference
                .module("main")
                .is_some_and(|m| m.contains_key("MainKt")),
            "module main must emit its facade"
        );
        // Flattening collapses the collision; the module-qualified inventory does not.
        assert!(
            reference.flatten().len() < reference.class_count(),
            "the duplicate p/Foo proves module identity is preserved beyond the bare name"
        );
    }

    #[test]
    fn mixed_java_contributes_java_classes_and_excludes_module_metadata() {
        if !kotlinc_available() {
            return;
        }
        let src = "// FILE: Helper.java\n\
                   public class Helper { public static String value() { return \"OK\"; } }\n\
                   // FILE: box.kt\n\
                   fun box(): String = Helper.value()\n";
        let reference = compile_or_skip(
            src,
            "box",
            &[common::stdlib_jar()],
            BoxJdk::Full { root: None },
        );
        let Some(reference) = reference else {
            return;
        };
        let main = reference
            .module(MAIN_MODULE)
            .expect("the single module must be `main`");
        assert!(
            main.contains_key("Helper"),
            "the javac-emitted Java class must count: {:?}",
            main.keys()
        );
        assert!(
            main.contains_key("BoxKt"),
            "the Kotlin facade must count: {:?}",
            main.keys()
        );
        assert!(
            reference
                .qualified()
                .all(|((_, name), _)| !name.ends_with(".kotlin_module")),
            "non-class artifacts such as .kotlin_module are not in the population"
        );
    }

    #[test]
    fn a_compile_rejection_is_an_explicit_error() {
        if !kotlinc_available() {
            return;
        }
        let error = reference_compile(
            "fun box(): String { return 1 }\n",
            "Broken",
            &[],
            BoxJdk::Full { root: None },
            NO_HELPERS,
        )
        .expect_err("invalid Kotlin must be an explicit reference failure, not a dropped case");
        assert!(
            error.contains("error") || error.contains("Broken") || error.contains("module"),
            "the failure must identify a cause: {error}"
        );
    }

    fn compile_or_skip(
        src: &str,
        stem: &str,
        cp_jars: &[PathBuf],
        jdk: BoxJdk<'_>,
    ) -> Option<ReferenceClasses> {
        match reference_compile(src, stem, cp_jars, jdk, NO_HELPERS) {
            Ok(reference) => Some(reference),
            Err(error) if error == "kotlinc unavailable" => None,
            Err(error) => panic!("reference compiler rejected a valid fixture: {error}"),
        }
    }
}
