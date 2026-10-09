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
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use krusty::conformance::{
    directive, inject_support_module, module_units, reference_only_kotlinc_arguments, split_files,
    split_modules, unit_compiler_configuration, BoxJdk, FragmentUnit,
};

use super::common::{self, byte_dump};

/// The stable module identity for every single-module topology (ordinary, `// FILE:` multi-file,
/// and mixed Java). It matches the module name krusty emits those cases under.
pub const MAIN_MODULE: &str = "main";

/// Bump when the on-disk cache layout or the comparison population changes — invalidates every
/// cached entry. `v2` added per-module nesting and Java `.class` artifacts to `v1`'s flat tree.
/// `v3` strips kotlinc diagnostic-test markers from the reference source, so marker-bearing cases
/// that `v2` cached as FAILED must recompile. `v4` compiles folded multiplatform units as HMPP
/// fragments instead of a flat `-Xcommon-sources` split (changing even a successful MPP case's
/// bytes-exact invocation), so cases that `v3` cached must recompile. `v5` compiles each unit under
/// its own `// LAMBDAS:`/`// SAM_CONVERSIONS:`/`// JVM_DEFAULT_MODE:` modes and keys on the producing
/// JDK, so an older entry compiled under kotlinc's default modes or another JDK is never replayed.
/// Every directive-selected argument rides its unit's `unit_arguments` and the test-only feature
/// property rides `jvm_properties`, so a change of either re-keys itself.
const REF_CACHE_SALT: &str = "ref-classes-v6-typed-language-settings";

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
/// keyed by every input (source, stem, helpers, classpath/JDK arguments, each unit's code-generation
/// modes, the exact compiler identity, and the producing JDK's identity), so a re-run pays only for
/// cases whose inputs changed and never replays one exact version's or one JDK's bytes for another.
/// A transient failure (driver crash, work-dir clobber) and a snapshot/dev/beta compiler are not
/// cached.
pub fn reference_compile(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk: BoxJdk<'_>,
    coroutine_helpers: &str,
) -> Result<ReferenceClasses, String> {
    let units = plan_units(src, stem, coroutine_helpers)?;
    let jvm_properties = reference_jvm_properties(&units);
    let fingerprint = cache_key(
        src,
        stem,
        cp_jars,
        jdk,
        coroutine_helpers,
        &jvm_properties,
        &units,
    )?;
    let cache = reference_cache_dir(fingerprint);
    // A snapshot/dev/beta compiler is not an immutable build, so its output is never cached.
    let cacheable = byte_dump::published_compiler_id().is_some();
    if cacheable {
        let lookup = Instant::now();
        let cached = load_cache(&cache);
        STATS.cache_read.add_since(lookup);
        if let Some(cached) = cached {
            match &cached {
                Ok(_) => STATS.hits.bump(),
                Err(_) => STATS.cached_rejections.bump(),
            }
            return cached;
        }
    }

    STATS.misses.bump();
    let compiling = Instant::now();
    let scratch = ScratchDir::new()?;
    let result = compile_all(&units, cp_jars, jdk, &jvm_properties, scratch.path());
    STATS.compile.add_since(compiling);
    let cleanup = Instant::now();
    drop(scratch);
    STATS.cleanup.add_since(cleanup);
    if cacheable {
        let storing = Instant::now();
        store_cache(&cache, fingerprint, &result);
        STATS.store.add_since(storing);
    }
    result
}

/// The cache key [`reference_compile`] files this case's inventory under.
pub fn reference_cache_key(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk: BoxJdk<'_>,
    coroutine_helpers: &str,
) -> Result<u128, String> {
    let units = plan_units(src, stem, coroutine_helpers)?;
    let jvm_properties = reference_jvm_properties(&units);
    cache_key(
        src,
        stem,
        cp_jars,
        jdk,
        coroutine_helpers,
        &jvm_properties,
        &units,
    )
}

fn cache_key(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk: BoxJdk<'_>,
    coroutine_helpers: &str,
    jvm_properties: &[String],
    units: &[PlannedUnit],
) -> Result<u128, String> {
    let base_args = jdk.kotlinc_args(cp_jars)?;
    let unit_arguments: Vec<(&str, &[String])> = units
        .iter()
        .map(|unit| (unit.module.as_str(), unit.arguments.as_slice()))
        .collect();
    let unit_language_settings: Vec<_> = units
        .iter()
        .map(|unit| {
            (
                unit.module.as_str(),
                unit.language_version,
                unit.api_version,
            )
        })
        .collect();
    let (compiler_id, compiler_len) = compiler_identity();
    let producing_jdk = producing_jdk_identity()?;
    Ok(ReferenceCacheInputs {
        src,
        stem,
        coroutine_helpers,
        base_args: &base_args,
        jvm_properties,
        unit_arguments: &unit_arguments,
        unit_language_settings: &unit_language_settings,
        compiler_id: compiler_id.as_deref(),
        compiler_len,
        producing_jdk,
    }
    .fingerprint())
}

/// The selected producing JDK's identity, read once per process: the reference kotlinc servers and
/// javac this process starts all run on that one installation.
fn producing_jdk_identity() -> Result<&'static [u8], String> {
    static IDENTITY: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
    IDENTITY
        .get_or_init(|| {
            let home = common::selected_java_home().ok_or_else(|| {
                "no reference JDK is selected: set KRUSTY_REF_JAVA_HOME or JAVA_HOME".to_string()
            })?;
            common::producing_jdk::jdk_identity(Path::new(&home))
        })
        .as_deref()
        .map_err(Clone::clone)
}

/// The on-disk directory of one reference inventory cache entry.
pub fn reference_cache_dir(fingerprint: u128) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("target/cache/ref-classes/{fingerprint:032x}"))
}

/// Process-wide reference-inventory cache and compile totals, for telling a cold run's cache misses
/// and compile cost from a warm run's cache reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReferenceStats {
    /// Lookups answered by a published inventory.
    pub hits: u64,
    /// Lookups answered by a cached deterministic compile rejection.
    pub cached_rejections: u64,
    /// Lookups that compiled the case with the reference compiler.
    pub misses: u64,
    /// Time spent reading cached entries, hits and misses alike.
    pub cache_read: Duration,
    /// Time spent compiling missed cases (writing sources, kotlinc, javac, reading classes).
    pub compile: Duration,
    /// Time spent deleting missed cases' scratch directories.
    pub cleanup: Duration,
    /// Time spent publishing missed cases' results to the cache.
    pub store: Duration,
}

struct Counter(AtomicU64);

impl Counter {
    const fn new() -> Self {
        Counter(AtomicU64::new(0))
    }

    fn bump(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    fn add_since(&self, since: Instant) {
        let nanos = u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.0.fetch_add(nanos, Ordering::Relaxed);
    }

    fn count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    fn duration(&self) -> Duration {
        Duration::from_nanos(self.count())
    }
}

struct Counters {
    hits: Counter,
    cached_rejections: Counter,
    misses: Counter,
    cache_read: Counter,
    compile: Counter,
    cleanup: Counter,
    store: Counter,
}

static STATS: Counters = Counters {
    hits: Counter::new(),
    cached_rejections: Counter::new(),
    misses: Counter::new(),
    cache_read: Counter::new(),
    compile: Counter::new(),
    cleanup: Counter::new(),
    store: Counter::new(),
};

/// A snapshot of [`ReferenceStats`] for this process.
pub fn reference_stats() -> ReferenceStats {
    ReferenceStats {
        hits: STATS.hits.count(),
        cached_rejections: STATS.cached_rejections.count(),
        misses: STATS.misses.count(),
        cache_read: STATS.cache_read.duration(),
        compile: STATS.compile.duration(),
        cleanup: STATS.cleanup.duration(),
        store: STATS.store.duration(),
    }
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

/// Every input that can change one case's reference bytes. [`Self::fingerprint`] is free of I/O so
/// version, mode, and JDK isolation are directly testable.
#[derive(Clone, Copy)]
pub struct ReferenceCacheInputs<'a> {
    pub src: &'a str,
    pub stem: &'a str,
    pub coroutine_helpers: &'a str,
    /// The case's classpath and JDK arguments.
    pub base_args: &'a [String],
    /// The JVM properties the reference compiler server runs under ([`reference_jvm_properties`]).
    pub jvm_properties: &'a [String],
    /// Each compilation unit's module and its directive-selected arguments, in build order.
    pub unit_arguments: &'a [(&'a str, &'a [String])],
    /// Exact language/API levels installed through kotlinc's typed configuration channel.
    pub unit_language_settings: &'a [(
        &'a str,
        Option<krusty::language_version::LanguageVersion>,
        Option<krusty::language_version::LanguageVersion>,
    )],
    pub compiler_id: Option<&'a str>,
    pub compiler_len: u64,
    /// The JDK the reference kotlinc and javac run on, from
    /// [`common::producing_jdk::jdk_identity`].
    pub producing_jdk: &'a [u8],
}

impl ReferenceCacheInputs<'_> {
    /// The cache key.
    pub fn fingerprint(&self) -> u128 {
        let mut parts: Vec<&[u8]> = vec![
            REF_CACHE_SALT.as_bytes(),
            self.src.as_bytes(),
            self.stem.as_bytes(),
            self.coroutine_helpers.as_bytes(),
        ];
        let base_count = (self.base_args.len() as u64).to_le_bytes();
        parts.push(&base_count);
        parts.extend(self.base_args.iter().map(|arg| arg.as_bytes()));
        let property_count = (self.jvm_properties.len() as u64).to_le_bytes();
        parts.push(&property_count);
        parts.extend(self.jvm_properties.iter().map(|arg| arg.as_bytes()));
        let unit_counts: Vec<[u8; 8]> = self
            .unit_arguments
            .iter()
            .map(|(_, args)| (args.len() as u64).to_le_bytes())
            .collect();
        for ((module, args), count) in self.unit_arguments.iter().zip(&unit_counts) {
            parts.push(module.as_bytes());
            parts.push(count);
            parts.extend(args.iter().map(|arg| arg.as_bytes()));
        }
        let setting_bytes: Vec<[u8; 8]> = self
            .unit_language_settings
            .iter()
            .map(|(_, language, api)| {
                let [language_major, language_minor] = language
                    .map(|version| [version.major, version.minor])
                    .unwrap_or([u16::MAX, u16::MAX]);
                let [api_major, api_minor] = api
                    .map(|version| [version.major, version.minor])
                    .unwrap_or([u16::MAX, u16::MAX]);
                let mut bytes = [0; 8];
                bytes[0..2].copy_from_slice(&language_major.to_le_bytes());
                bytes[2..4].copy_from_slice(&language_minor.to_le_bytes());
                bytes[4..6].copy_from_slice(&api_major.to_le_bytes());
                bytes[6..8].copy_from_slice(&api_minor.to_le_bytes());
                bytes
            })
            .collect();
        for ((module, _, _), settings) in self.unit_language_settings.iter().zip(&setting_bytes) {
            parts.push(module.as_bytes());
            parts.push(settings);
        }
        // The exact compiler identity isolates one release/RC's bytes from another's; the jar length
        // guards a same-identity rebuild. An unpublished compiler never reaches the cache, but still
        // contributes a stable key so an in-run second lookup matches.
        parts.push(self.compiler_id.unwrap_or("").as_bytes());
        let compiler_len = self.compiler_len.to_le_bytes();
        parts.push(&compiler_len);
        parts.push(self.producing_jdk);
        byte_dump::fingerprint_parts(&parts)
    }
}

/// kotlinc "test-only" language features: the compiler rejects `-XXLanguage:+<feature>` for these
/// from the command line unless the `kotlinc.test.allow.testonly.language.features` JVM property is
/// set, exactly as kotlinc's own test runner enables them. A corpus case that opts one in through
/// `// LANGUAGE:` must reference-compile with that property, not be dropped from the population.
const TEST_ONLY_LANGUAGE_FEATURES: &[&str] = &["ImplicitSignedToUnsignedIntegerConversion"];

/// The JVM properties the reference compiler server runs under for these units: the
/// `kotlinc.test.allow.testonly.language.features` property when a unit's arguments enable a
/// test-only feature, which the persistent server applies on its own property-keyed pool rather
/// than passing to the compiler. Every other directive is an argument of the unit
/// ([`unit_compiler_configuration`]), shared with krusty's own compile of the case.
fn reference_jvm_properties(units: &[PlannedUnit]) -> Vec<String> {
    let enables_test_only_feature = units
        .iter()
        .flat_map(|unit| &unit.arguments)
        .any(|argument| {
            argument
                .strip_prefix("-XXLanguage:+")
                .is_some_and(|feature| TEST_ONLY_LANGUAGE_FEATURES.contains(&feature))
        });
    if enables_test_only_feature {
        vec!["-Dkotlinc.test.allow.testonly.language.features=true".to_string()]
    } else {
        Vec::new()
    }
}

fn compiler_identity() -> (Option<String>, u64) {
    let identity = byte_dump::published_compiler_id();
    let len = common::kotlin_compiler_jar()
        .and_then(|jar| std::fs::metadata(jar).ok())
        .map_or(0, |metadata| metadata.len());
    (identity, len)
}

/// One reference compilation unit of a case, in build order: its module identity, sources, the
/// earlier units it reads, and the kotlinc arguments its directives select.
/// `fragments` is the folded `dependsOn` chain in dependency-first order (empty for a single-module
/// case); a multi-fragment unit compiles as HMPP, each fragment owning a contiguous run of the
/// Kotlin blocks in that same order.
struct PlannedUnit {
    module: String,
    kotlin: Vec<(String, String)>,
    java: Vec<(String, String)>,
    fragments: Vec<FragmentUnit>,
    deps: Vec<String>,
    friends: Vec<String>,
    arguments: Vec<String>,
    language_version: Option<krusty::language_version::LanguageVersion>,
    api_version: Option<krusty::language_version::LanguageVersion>,
}

/// Split a case into the units krusty compiles it as: a `// MODULE:` build's folded units, else
/// one `main` unit of its `// FILE:` blocks or of the whole source. `// WITH_COROUTINES` adds the
/// generated helpers exactly where krusty does. Each unit's arguments come from
/// [`unit_compiler_configuration`], the selection the gate's own compile of that unit parses,
/// followed by
/// the case's [`reference_only_kotlinc_arguments`].
fn plan_units(src: &str, stem: &str, coroutine_helpers: &str) -> Result<Vec<PlannedUnit>, String> {
    let reference_only = reference_only_kotlinc_arguments(src)
        .map_err(|error| format!("reference oracle: {error}"))?;
    let configuration = |kotlin: &[(String, String)]| {
        unit_compiler_configuration(src, kotlin.iter().map(|(_, source)| source.as_str()))
            .map(|mut configuration| {
                configuration
                    .arguments
                    .extend(reference_only.iter().cloned());
                configuration
            })
            .map_err(|error| format!("reference oracle: {error}"))
    };
    if src.contains("// MODULE:") {
        let Some(mut modules) = split_modules(src) else {
            return Err("reference oracle does not support this // MODULE: shape".to_string());
        };
        if directive(src, "WITH_COROUTINES") {
            inject_support_module(&mut modules, coroutine_helpers);
        }
        return module_units(&modules)
            .into_iter()
            .map(|unit| {
                let configuration = configuration(&unit.files)?;
                Ok(PlannedUnit {
                    arguments: configuration.arguments,
                    language_version: configuration.language_version,
                    api_version: configuration.api_version,
                    module: unit.name,
                    kotlin: unit.files,
                    java: unit.java_files,
                    fragments: unit.fragments,
                    deps: unit.deps,
                    friends: unit.friends,
                })
            })
            .collect();
    }
    let (mut kotlin, java) = if src.contains("// FILE:") {
        split_files(src)
    } else {
        (vec![(stem.to_string(), src.to_string())], Vec::new())
    };
    if directive(src, "WITH_COROUTINES") {
        kotlin.push(("CoroutineUtil".to_string(), coroutine_helpers.to_string()));
    }
    let configuration = configuration(&kotlin)?;
    Ok(vec![PlannedUnit {
        arguments: configuration.arguments,
        language_version: configuration.language_version,
        api_version: configuration.api_version,
        module: MAIN_MODULE.to_string(),
        kotlin,
        java,
        fragments: Vec::new(),
        deps: Vec::new(),
        friends: Vec::new(),
    }])
}

fn compile_all(
    units: &[PlannedUnit],
    cp_jars: &[PathBuf],
    jdk: BoxJdk<'_>,
    jvm_properties: &[String],
    work: &Path,
) -> Result<ReferenceClasses, String> {
    let mut classes = ReferenceClasses::default();
    let mut outputs: HashMap<&str, PathBuf> = HashMap::new();
    for unit in units {
        let mut classpath = cp_jars.to_vec();
        let mut friends = Vec::new();
        for dependency in &unit.deps {
            let Some(output) = outputs.get(dependency.as_str()) else {
                return Err(format!(
                    "reference module {}: dependency {dependency} was not built",
                    unit.module
                ));
            };
            classpath.push(output.clone());
            if unit.friends.contains(dependency) {
                friends.push(output.clone());
            }
        }
        let output = work.join(&unit.module).join("classes");
        let unit_classes = compile_unit(
            &Unit {
                module: &unit.module,
                kotlin: &unit.kotlin,
                java: &unit.java,
                fragments: &unit.fragments,
                arguments: &unit.arguments,
                language_version: unit.language_version,
                api_version: unit.api_version,
                classpath: &classpath,
                friend_paths: &friends,
            },
            jdk,
            jvm_properties,
            &work.join(&unit.module),
            &output,
        )?;
        for (name, bytes) in unit_classes {
            classes.insert(&unit.module, &name, bytes);
        }
        outputs.insert(&unit.module, output);
    }
    if classes.is_empty() {
        return Err("reference compile produced no .class artifacts".to_string());
    }
    Ok(classes)
}

/// One planned unit handed to the reference compiler with the classpath its built dependencies
/// resolved to.
struct Unit<'a> {
    module: &'a str,
    kotlin: &'a [(String, String)],
    java: &'a [(String, String)],
    fragments: &'a [FragmentUnit],
    arguments: &'a [String],
    language_version: Option<krusty::language_version::LanguageVersion>,
    api_version: Option<krusty::language_version::LanguageVersion>,
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
    jvm_properties: &[String],
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
        args.extend_from_slice(jvm_properties);
        args.extend_from_slice(unit.arguments);
        args.push("-module-name".to_string());
        args.push(unit.module.to_string());
        // A folded `dependsOn` chain compiles as hierarchical multiplatform: one fragment per
        // source set, with `refines` edges, so an `actual` in an intermediate source set matches an
        // `expect` in a refined-upon one. The flat `-Xcommon-sources` common/platform split cannot
        // model that and rejects an intermediate `actual` as having no `expect`.
        if unit.fragments.len() > 1 {
            args.push("-Xmulti-platform".to_string());
            let mut offset = 0;
            for fragment in unit.fragments {
                args.push(format!("-Xfragments={}", fragment.name));
                for path in &kotlin_paths[offset..offset + fragment.kotlin_file_count] {
                    args.push(format!(
                        "-Xfragment-sources={}:{}",
                        fragment.name,
                        path.to_string_lossy()
                    ));
                }
                for refined in &fragment.refines {
                    args.push(format!("-Xfragment-refines={}:{refined}", fragment.name));
                }
                offset += fragment.kotlin_file_count;
            }
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
        // This inventory's own cache is keyed by every input and the exact compiler, so a miss
        // compiles live rather than also replaying or recording through the recorded-byte archive.
        let (code, diagnostics) =
            common::kotlinc_server::kotlinc_compile_unrecorded_with_language_settings(
                &args,
                unit.language_version
                    .map(|version| version.to_string())
                    .as_deref(),
                unit.api_version
                    .map(|version| version.to_string())
                    .as_deref(),
            )
            .ok_or_else(|| "kotlinc unavailable".to_string())?;
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
            let (file, contents) = if name.ends_with(".java") {
                (block_dir.join(leaf), source.clone())
            } else {
                // kotlinc rejects the raw diagnostic-test markers krusty's lexer consumes as trivia,
                // so strip them for the reference exactly as kotlinc's own test runner does.
                (
                    block_dir.join(format!("{leaf}.kt")),
                    strip_diagnostic_markers(source),
                )
            };
            std::fs::write(&file, &contents)
                .map_err(|error| format!("cannot write reference source: {error}"))?;
            Ok(file)
        })
        .collect()
}

/// Strip kotlinc diagnostic-test markers — `<!DIAGNOSTIC_NAME!>` (open) and `<!>` (close) — the same
/// way krusty's lexer consumes them as trivia (`src/lexer.rs`), so the reference compiler sees the
/// program krusty actually compiled rather than failing on the raw markers (kotlinc's own box-test
/// runner strips them identically before compiling).
///
/// An open marker is recognized only when an upper-snake-case name follows `<!` AND a closing `!>`
/// appears on the same line, so a real `a < !b` is left intact. Markers never span a line, so
/// removing them preserves line numbers — and thus `LineNumberTable` bytes — on both sides.
fn strip_diagnostic_markers(src: &str) -> String {
    let b = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' && i + 2 < b.len() && b[i + 1] == b'!' && b[i + 2] == b'>' {
            i += 3; // close marker `<!>`
            continue;
        }
        if b[i] == b'<'
            && i + 2 < b.len()
            && b[i + 1] == b'!'
            && (b[i + 2].is_ascii_uppercase() || b[i + 2] == b'_')
        {
            let mut j = i + 2;
            while j + 1 < b.len() && b[j] != b'\n' && !(b[j] == b'!' && b[j + 1] == b'>') {
                j += 1;
            }
            if j + 1 < b.len() && b[j] == b'!' && b[j + 1] == b'>' {
                i = j + 2; // consume through `!>`
                continue;
            }
        }
        // A real `<` or any other byte (including UTF-8 continuation bytes, which are never `<`/`!`)
        // is copied verbatim.
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).expect("stripping ASCII markers preserves UTF-8")
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

    /// A plain case's key inputs, which each test varies one field of.
    fn key_inputs<'a>(
        base_args: &'a [String],
        unit_arguments: &'a [(&'a str, &'a [String])],
    ) -> ReferenceCacheInputs<'a> {
        ReferenceCacheInputs {
            src: "fun box() = \"OK\"",
            stem: "Box",
            coroutine_helpers: NO_HELPERS,
            base_args,
            jvm_properties: &[],
            unit_arguments,
            unit_language_settings: &[(MAIN_MODULE, None, None)],
            compiler_id: Some("2.4.20"),
            compiler_len: 100,
            producing_jdk: b"JAVA_VERSION=\"21.0.12\"",
        }
    }

    fn classpath(jar: &str) -> Vec<String> {
        vec!["-classpath".to_string(), jar.to_string()]
    }

    const PLAIN_UNIT: &[(&str, &[String])] = &[(MAIN_MODULE, &[])];

    #[test]
    fn cache_key_isolates_version_source_and_classpath() {
        let a_jar = classpath("/a.jar");
        let base = key_inputs(&a_jar, PLAIN_UNIT);
        // Same inputs → same key.
        assert_eq!(
            base.fingerprint(),
            key_inputs(&a_jar, PLAIN_UNIT).fingerprint()
        );
        // A different exact compiler version must not replay the first version's bytes.
        assert_ne!(
            base.fingerprint(),
            ReferenceCacheInputs {
                compiler_id: Some("2.4.10"),
                ..base
            }
            .fingerprint()
        );
        // A changed source, classpath, or jar length each changes the key.
        assert_ne!(
            base.fingerprint(),
            ReferenceCacheInputs {
                src: "fun box() = \"FAIL\"",
                ..base
            }
            .fingerprint()
        );
        let b_jar = classpath("/b.jar");
        assert_ne!(
            base.fingerprint(),
            key_inputs(&b_jar, PLAIN_UNIT).fingerprint()
        );
        assert_ne!(
            base.fingerprint(),
            ReferenceCacheInputs {
                compiler_len: 101,
                ..base
            }
            .fingerprint()
        );
    }

    #[test]
    fn cache_key_isolates_unit_modes_and_the_producing_jdk() {
        let a_jar = classpath("/a.jar");
        let base = key_inputs(&a_jar, PLAIN_UNIT);
        let class_lambdas = ["-Xlambdas=class".to_string()];
        let no_compatibility = ["-jvm-default".to_string(), "no-compatibility".to_string()];
        // A unit compiled under a non-default mode is a different reference compile.
        for unit_arguments in [
            &[(MAIN_MODULE, class_lambdas.as_slice())][..],
            &[(MAIN_MODULE, no_compatibility.as_slice())][..],
        ] {
            assert_ne!(
                base.fingerprint(),
                key_inputs(&a_jar, unit_arguments).fingerprint()
            );
        }
        // The same mode on a different unit of the same build is a different reference compile.
        let on_lib: &[(&str, &[String])] = &[("lib", &no_compatibility), ("main", &[])];
        let on_main: &[(&str, &[String])] = &[("lib", &[]), ("main", &no_compatibility)];
        assert_ne!(
            key_inputs(&a_jar, on_lib).fingerprint(),
            key_inputs(&a_jar, on_main).fingerprint()
        );
        // Another producing JDK never replays this JDK's Java classes.
        assert_ne!(
            base.fingerprint(),
            ReferenceCacheInputs {
                producing_jdk: b"JAVA_VERSION=\"25.0.4\"",
                ..base
            }
            .fingerprint()
        );
    }

    #[test]
    fn cache_key_isolates_typed_language_and_api_levels() {
        let jar = classpath("/a.jar");
        let base = key_inputs(&jar, PLAIN_UNIT);
        let historical = [(
            MAIN_MODULE,
            None,
            Some(krusty::language_version::LanguageVersion::new(1, 8)),
        )];
        assert_ne!(
            base.fingerprint(),
            ReferenceCacheInputs {
                unit_language_settings: &historical,
                ..base
            }
            .fingerprint()
        );
    }

    #[test]
    fn typed_historical_levels_control_the_reference_compile() {
        if !kotlinc_available() {
            return;
        }
        let root = common::scratch_dir().expect("allocate a compile directory");
        let api_source = root.join("HistoricalApi.kt");
        std::fs::write(
            &api_source,
            "enum class Entry { VALUE }\nfun selected() = Entry.entries\n",
        )
        .expect("write the API source");
        let language_source = root.join("HistoricalLanguage.kt");
        std::fs::write(&language_source, "class Selected\n").expect("write the language source");
        let compile = |source: &Path, output: &str, language, api| {
            common::kotlinc_server::kotlinc_compile_unrecorded_with_language_settings(
                &[
                    "-d".to_string(),
                    root.join(output).to_string_lossy().into_owned(),
                    source.to_string_lossy().into_owned(),
                ],
                language,
                api,
            )
            .expect("reference compiler is provisioned")
        };
        let (old_api_code, _) = compile(&api_source, "api-1.8", None, Some("1.8"));
        let (new_api_code, new_api_diagnostics) =
            compile(&api_source, "api-1.9", None, Some("1.9"));
        let (old_language_code, old_language_diagnostics) =
            compile(&language_source, "language-1.4", Some("1.4"), Some("1.4"));
        let (new_language_code, new_language_diagnostics) =
            compile(&language_source, "language-1.5", Some("1.5"), Some("1.5"));
        let old_language_class =
            std::fs::read(root.join("language-1.4/Selected.class")).expect("read 1.4 class");
        let new_language_class =
            std::fs::read(root.join("language-1.5/Selected.class")).expect("read 1.5 class");
        let _ = std::fs::remove_dir_all(&root);
        assert_ne!(old_api_code, 0, "API 1.8 must hide Enum.entries");
        assert_eq!((new_api_code, new_api_diagnostics.as_str()), (0, ""));
        assert_eq!(
            (old_language_code, old_language_diagnostics.as_str()),
            (0, "")
        );
        assert_eq!(
            (new_language_code, new_language_diagnostics.as_str()),
            (0, "")
        );
        assert_ne!(
            old_language_class, new_language_class,
            "the exact language level must reach kotlinc's metadata stamp"
        );
    }

    #[test]
    fn each_planned_unit_selects_its_own_codegen_modes() {
        let args = |src: &str, stem: &str| {
            plan_units(src, stem, NO_HELPERS)
                .expect("a supported shape")
                .into_iter()
                .map(|unit| (unit.module, unit.arguments))
                .collect::<Vec<_>>()
        };
        let strings = |args: &[&str]| args.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            args("// LAMBDAS: CLASS\nfun box() = \"OK\"\n", "box"),
            vec![(MAIN_MODULE.to_string(), strings(&["-Xlambdas=class"]))]
        );
        assert_eq!(
            args(
                "// FILE: a.kt\n// SAM_CONVERSIONS: CLASS\nfun a() {}\n// FILE: b.kt\nfun box() = \"OK\"\n",
                "box"
            ),
            vec![(MAIN_MODULE.to_string(), strings(&["-Xsam-conversions=class"]))]
        );
        // A module's directive stays with the unit whose sources carry it.
        assert_eq!(
            args(
                "// MODULE: lib\n// FILE: lib.kt\n// JVM_DEFAULT_MODE: disable\ninterface I\n\
                 // MODULE: main(lib)\n// FILE: main.kt\nfun box() = \"OK\"\n",
                "main"
            ),
            vec![
                ("lib".to_string(), strings(&["-jvm-default", "disable"])),
                ("main".to_string(), Vec::new()),
            ]
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

    #[test]
    fn diagnostic_markers_are_stripped_but_real_less_than_is_kept() {
        // Open marker with an upper-snake name and a same-line `!>`, plus a bare close marker.
        assert_eq!(
            strip_diagnostic_markers("1 + <!NON_TAIL_RECURSIVE_CALL!>f<!>(x)\n"),
            "1 + f(x)\n"
        );
        // A real `a < !b` (lowercase operand, no same-line `!>`) must survive untouched.
        assert_eq!(strip_diagnostic_markers("a < !b\n"), "a < !b\n");
        // Line counts are preserved so LineNumberTable bytes match on both sides.
        let marked = "val x =\n    <!UNUSED!>y<!>\n";
        let stripped = strip_diagnostic_markers(marked);
        assert_eq!(stripped, "val x =\n    y\n");
        assert_eq!(stripped.matches('\n').count(), marked.matches('\n').count());
        // Non-ASCII content is preserved verbatim.
        assert_eq!(
            strip_diagnostic_markers("val s = \"αβ\" <!FOO!>+ z<!>\n"),
            "val s = \"αβ\" + z\n"
        );
    }

    #[test]
    fn every_unit_compiles_under_the_shared_directive_arguments() {
        let arguments = |src: &str| {
            plan_units(src, "box", NO_HELPERS).map(|units| {
                units
                    .into_iter()
                    .map(|unit| unit.arguments)
                    .collect::<Vec<_>>()
            })
        };
        let strings = |args: &[&str]| args.iter().map(ToString::to_string).collect::<Vec<_>>();
        // Case-wide arguments reach every unit; a unit's code-generation modes only that unit.
        assert_eq!(
            arguments(
                "// ALLOW_KOTLIN_PACKAGE\n// ASSERTIONS_MODE: always-enable\n\
                 // MODULE: lib\n// FILE: lib.kt\n// LAMBDAS: CLASS\npackage kotlin.jvm\nfun a() {}\n\
                 // MODULE: main(lib)\n// FILE: main.kt\nfun box() = \"OK\"\n"
            ),
            Ok(vec![
                strings(&[
                    "-Xallow-kotlin-package",
                    "-Xassertions=always-enable",
                    "-Xlambdas=class"
                ]),
                strings(&["-Xallow-kotlin-package", "-Xassertions=always-enable"]),
            ])
        );
        // A directive with no kotlinc spelling fails the reference closed rather than compiling the
        // case under a default it did not ask for.
        assert_eq!(
            arguments("// FILE: a.kt\n// LAMBDAS: SIDEWAYS\nfun box() = \"OK\"\n"),
            Err(
                "reference oracle: box directive `// LAMBDAS: SIDEWAYS` names no code-generation mode"
                    .to_string()
            )
        );
    }

    #[test]
    fn only_a_test_only_feature_runs_the_reference_under_the_property() {
        let properties = |src: &str| {
            reference_jvm_properties(
                &plan_units(src, "box", NO_HELPERS).expect("a supported shape"),
            )
        };
        assert_eq!(
            properties(
                "// LANGUAGE: +MultiPlatformProjects +ImplicitSignedToUnsignedIntegerConversion\n\
                 // ALLOW_KOTLIN_PACKAGE\nfun box() = \"OK\"\n"
            ),
            vec!["-Dkotlinc.test.allow.testonly.language.features=true".to_string()]
        );
        // An ordinary feature must NOT drag in the property: that would needlessly spin a
        // property-keyed compiler server and change the key for an ordinary case.
        assert_eq!(
            properties("// LANGUAGE: +ContextParameters\nfun box() = \"OK\"\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn directive_reference_options_change_the_cache_key() {
        // Directive arguments and the test-only property both move the reference cache key: a
        // cached plain compile can never be replayed as the directive-enabled one.
        let a_jar = classpath("/a.jar");
        let allow = ["-Xallow-kotlin-package".to_string()];
        let key = |jvm_properties: &[String], unit: &[String]| {
            ReferenceCacheInputs {
                src: "package kotlin.jvm\nfun box() = \"OK\"\n",
                jvm_properties,
                ..key_inputs(&a_jar, &[(MAIN_MODULE, unit)])
            }
            .fingerprint()
        };
        let plain = key(&[], &[]);
        assert_ne!(plain, key(&[], &allow));
        assert_ne!(
            plain,
            key(
                &["-Dkotlinc.test.allow.testonly.language.features=true".to_string()],
                &[]
            )
        );
    }

    #[test]
    fn a_kotlin_package_case_reference_compiles_with_the_allow_flag() {
        if !kotlinc_available() {
            return;
        }
        // `package kotlin.jvm` is rejected ("only the Kotlin standard library is allowed to use the
        // 'kotlin' package") unless `-Xallow-kotlin-package` is passed, which the directive opts in.
        let src = "// ALLOW_KOTLIN_PACKAGE\n\
                   // FILE: box.kt\n\
                   package kotlin.jvm\n\
                   class Marker\n\
                   fun box(): String { Marker(); return \"OK\" }\n";
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
            main.contains_key("kotlin/jvm/Marker"),
            "the reserved-package class must compile: {:?}",
            main.keys()
        );
        assert!(
            main.contains_key("kotlin/jvm/BoxKt"),
            "the Kotlin facade (under the reserved package) must count: {:?}",
            main.keys()
        );
    }

    #[test]
    fn a_test_only_language_feature_reference_compiles_through_the_property_server() {
        if !kotlinc_available() {
            return;
        }
        // kotlinc refuses `-XXLanguage:+ImplicitSignedToUnsignedIntegerConversion` from the command
        // line ("test-only and cannot be enabled from command line") unless the JVM property is set;
        // the oracle enables it and content-scores the real classes rather than drop the case.
        let src = "// LANGUAGE: +ImplicitSignedToUnsignedIntegerConversion\n\
                   // FILE: box.kt\n\
                   fun box(): String = \"OK\"\n";
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
            main.contains_key("BoxKt"),
            "the facade must compile under the test-only feature: {:?}",
            main.keys()
        );
    }

    #[test]
    fn an_hmpp_intermediate_actual_reference_compiles_as_fragments() {
        if !kotlinc_available() {
            return;
        }
        // The real `multiplatform/k2/hmpp/inheritanceFromExpectedInApp3-3` topology: a three-level
        // `dependsOn` hierarchy where an `actual` lives in an INTERMEDIATE source set (lib-inter
        // actualizes lib-common's `expect LibClass1`). The flat `-Xcommon-sources` common/platform
        // split rejects that ("has no corresponding 'expect' declaration"); the fragment model
        // matches `actual` and `expect` across refined source sets.
        let src = "// LANGUAGE: +MultiPlatformProjects\n\
                   // MODULE: lib-common\n\
                   // FILE: lib-common.kt\n\
                   expect open class LibClass1() { open fun foo(): String }\n\
                   // MODULE: lib-inter()()(lib-common)\n\
                   // FILE: lib-inter.kt\n\
                   expect open class LibInterClass() { open fun foo(): String }\n\
                   actual open class LibClass1 { actual open fun foo(): String = \"1\"; fun baz() {} }\n\
                   // MODULE: lib-platform()()(lib-inter)\n\
                   // FILE: lib-platform.kt\n\
                   actual open class LibInterClass { actual open fun foo(): String = \"3\"; fun baz() {} }\n\
                   // MODULE: app-common(lib-common)\n\
                   // FILE: app-common.kt\n\
                   class AppLibClass1 : LibClass1() { override fun foo(): String = \"AppCommon1\" }\n\
                   // MODULE: app-inter(lib-inter)()(app-common)\n\
                   // FILE: app-inter.kt\n\
                   class AppInterCommon : LibInterClass() { override fun foo(): String = \"AppInterCommon\" }\n\
                   // MODULE: app-platform(lib-platform)()(app-inter)\n\
                   // FILE: app-platform.kt\n\
                   class AppInterPlatform : LibInterClass() { override fun foo(): String = \"AppInterPlatform\"; fun extra() = baz() }\n\
                   fun box(): String {\n\
                       if (LibClass1().foo() != \"1\") return \"FAIL\"\n\
                       if (LibInterClass().foo() != \"3\") return \"FAIL\"\n\
                       if (AppLibClass1().foo() != \"AppCommon1\") return \"FAIL\"\n\
                       if (AppInterCommon().foo() != \"AppInterCommon\") return \"FAIL\"\n\
                       if (AppInterPlatform().foo() != \"AppInterPlatform\") return \"FAIL\"\n\
                       return \"OK\"\n\
                   }\n";
        let reference = compile_or_skip(
            src,
            "box",
            &[common::stdlib_jar()],
            BoxJdk::Full { root: None },
        );
        let Some(reference) = reference else {
            return;
        };
        // The lib leaf unit folds lib-common + lib-inter + lib-platform; the intermediate `actual
        // LibClass1` and the leaf `actual LibInterClass` both emit under it.
        let lib = reference
            .module("lib-platform")
            .expect("lib-platform unit must emit classes");
        assert!(
            lib.contains_key("LibClass1") && lib.contains_key("LibInterClass"),
            "intermediate and leaf actuals must compile: {:?}",
            lib.keys()
        );
        // The app leaf unit folds app-common + app-inter + app-platform.
        let app = reference
            .module("app-platform")
            .expect("app-platform unit must emit classes");
        assert!(
            app.contains_key("AppLibClass1") && app.contains_key("AppInterPlatform"),
            "folded app classes must compile: {:?}",
            app.keys()
        );
    }

    /// Set only in the child process [`a_reference_miss_compiles_live_without_the_recorded_byte_archive`]
    /// spawns, naming the empty recorded-byte directory it must leave untouched.
    const ARCHIVE_PROBE: &str = "KRUSTY_REF_ARCHIVE_PROBE_DIR";

    #[test]
    fn a_reference_miss_compiles_live_without_the_recorded_byte_archive() {
        if !kotlinc_available() || std::env::var_os(ARCHIVE_PROBE).is_some() {
            return;
        }
        // A child process keeps the process-wide counters exact and the environment private. It
        // runs as CI does on a pull request: no recorded-byte entry for this case, no permission to
        // compile a missing one, and no permission to write the archive.
        let dumps = std::env::temp_dir().join(format!(
            "krusty_ref_archive_probe_{}_{}",
            std::process::id(),
            staging_sequence()
        ));
        let _ = std::fs::remove_dir_all(&dumps);
        std::fs::create_dir_all(&dumps).expect("create probe recorded-byte directory");
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "box_reference_classes::tests::reference_archive_probe",
                "--nocapture",
            ])
            .env(ARCHIVE_PROBE, &dumps)
            .env("KRUSTY_CLASS_DUMP_DIR", &dumps)
            .env("CI", "true")
            .env_remove("KRUSTY_CLASS_DUMP_COMPILE_MISSING")
            .env_remove("KRUSTY_CLASS_DUMP_WRITE")
            .env_remove("KRUSTY_RECORD")
            .env_remove("KRUSTY_RECORD_CLASS_DUMPS")
            .output()
            .expect("run the reference archive probe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "reference archive probe failed: {}\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let written: Vec<_> = std::fs::read_dir(&dumps)
            .expect("read probe recorded-byte directory")
            .map(|entry| entry.expect("probe directory entry").file_name())
            .collect();
        assert_eq!(written, Vec::<std::ffi::OsString>::new());
        std::fs::remove_dir_all(&dumps).expect("remove probe recorded-byte directory");
    }

    #[test]
    fn reference_archive_probe() {
        // Only an immutable release or RC build publishes inventory entries to read back.
        if std::env::var_os(ARCHIVE_PROBE).is_none() || byte_dump::published_compiler_id().is_none()
        {
            return;
        }
        // Unique source: the first lookup must miss the reference inventory cache.
        let src = format!(
            "// probe {} {:?}\nfun box(): String = \"OK\"\n",
            std::process::id(),
            std::time::SystemTime::now()
        );
        let jdk = BoxJdk::Full { root: None };
        let before = (
            reference_stats(),
            common::kotlinc_server::kotlinc_server_stats(),
        );
        let first = reference_compile(&src, "probe", &[], jdk, NO_HELPERS)
            .expect("a reference miss compiles live");
        let after_miss = (
            reference_stats(),
            common::kotlinc_server::kotlinc_server_stats(),
        );
        let second = reference_compile(&src, "probe", &[], jdk, NO_HELPERS)
            .expect("the published inventory is read back");
        let after_hit = (
            reference_stats(),
            common::kotlinc_server::kotlinc_server_stats(),
        );

        assert_eq!(before.0, ReferenceStats::default());
        assert_eq!(
            before.1,
            common::kotlinc_server::KotlincServerStats::default()
        );
        assert_eq!(first, second);
        assert!(first
            .module(MAIN_MODULE)
            .is_some_and(|classes| classes.contains_key("ProbeKt")));
        assert_eq!(
            (
                after_miss.0.hits,
                after_miss.0.cached_rejections,
                after_miss.0.misses
            ),
            (0, 0, 1)
        );
        assert_eq!(
            (
                after_miss.1.requests,
                after_miss.1.starts,
                after_miss.1.restarts
            ),
            (1, 1, 0)
        );
        assert_eq!(
            (
                after_hit.0.hits,
                after_hit.0.cached_rejections,
                after_hit.0.misses
            ),
            (1, 0, 1)
        );
        assert_eq!(
            after_hit.1, after_miss.1,
            "a cache hit never reaches kotlinc"
        );

        let cache = reference_cache_dir(
            reference_cache_key(&src, "probe", &[], jdk, NO_HELPERS).expect("probe cache key"),
        );
        assert!(cache.join("OK").is_file(), "missing {}", cache.display());
        std::fs::remove_dir_all(&cache).expect("remove the probe's inventory entry");
    }

    /// Set only in the child processes
    /// [`a_changed_producing_jdk_misses_the_reference_cache_across_processes`] spawns, carrying the
    /// unique tag of the mixed-Java case they compile.
    const JDK_PROBE: &str = "KRUSTY_REF_JDK_PROBE_TAG";

    /// A JDK home of another feature release than the selected one, for the cross-JDK half of the
    /// producing-JDK cache test.
    const SECOND_JDK: &str = "KRUSTY_SECOND_JAVA_HOME";

    /// One child process's reference lookup: its cache key, the process's hit and miss counts, and
    /// the class-file major version of the javac-compiled class it got.
    #[derive(Debug, PartialEq, Eq)]
    struct JdkProbeRun {
        key: u128,
        hits: u64,
        misses: u64,
        java_major: u16,
    }

    fn jdk_probe_source(tag: &str) -> String {
        format!(
            "// FILE: Greeting.java\n\
             public class Greeting {{ public static String ok() {{ return \"OK\"; }} }}\n\
             // FILE: box.kt\n\
             // {tag}\n\
             fun box(): String = Greeting.ok()\n"
        )
    }

    /// The class-file major version javac at `java_home` emits without `--release`.
    fn javac_major(java_home: &Path) -> u16 {
        let release = std::fs::read_to_string(java_home.join("release"))
            .unwrap_or_else(|error| panic!("read {}/release: {error}", java_home.display()));
        let feature: u16 = release
            .lines()
            .find_map(|line| line.strip_prefix("JAVA_VERSION="))
            .and_then(|version| version.trim_matches('"').split('.').next()?.parse().ok())
            .unwrap_or_else(|| panic!("no JAVA_VERSION in {}/release", java_home.display()));
        feature + 44
    }

    fn run_jdk_probe(java_home: &Path, tag: &str) -> JdkProbeRun {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "box_reference_classes::tests::producing_jdk_probe",
                "--nocapture",
            ])
            .env(JDK_PROBE, tag)
            .env("KRUSTY_REF_JAVA_HOME", java_home)
            .env("JAVA_HOME", java_home)
            .output()
            .expect("run the producing-JDK probe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "producing-JDK probe failed: {}\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let report = stdout
            .lines()
            .find_map(|line| line.strip_prefix("jdk-probe "))
            .unwrap_or_else(|| panic!("no probe report in {stdout}"));
        let field = |name: &str| {
            report
                .split(' ')
                .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
                .unwrap_or_else(|| panic!("no {name} in {report}"))
        };
        JdkProbeRun {
            key: u128::from_str_radix(field("key"), 16).expect("hex key"),
            hits: field("hits").parse().expect("hit count"),
            misses: field("misses").parse().expect("miss count"),
            java_major: field("major").parse().expect("major version"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_changed_producing_jdk_misses_the_reference_cache_across_processes() {
        if !kotlinc_available()
            || std::env::var_os(JDK_PROBE).is_some()
            || byte_dump::published_compiler_id().is_none()
        {
            return;
        }
        // Every probe process selects the JDK through one unchanging path, a symlink this test
        // retargets, so only the installation's own identity can tell the two JDKs apart.
        let root = std::env::temp_dir().join(format!(
            "krusty_ref_jdk_probe_{}_{}",
            std::process::id(),
            staging_sequence()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create the probe directory");
        let home = root.join("jdk");
        let first_jdk = PathBuf::from(common::java_home());
        std::os::unix::fs::symlink(&first_jdk, &home).expect("link the first JDK");
        let tag = format!(
            "producing-JDK probe {} {:?}",
            std::process::id(),
            std::time::SystemTime::now()
        );

        let cold = run_jdk_probe(&home, &tag);
        assert_eq!(
            (cold.hits, cold.misses, cold.java_major),
            (0, 1, javac_major(&first_jdk)),
            "a new case compiles live under the first JDK"
        );
        let warm = run_jdk_probe(&home, &tag);
        assert_eq!(
            warm,
            JdkProbeRun {
                hits: 1,
                misses: 0,
                ..cold
            },
            "another process on the same JDK reads the published inventory back"
        );

        let mut keys = vec![cold.key];
        match std::env::var_os(SECOND_JDK).filter(|home| !home.is_empty()) {
            Some(second_jdk) => {
                let second_jdk = PathBuf::from(second_jdk);
                assert_ne!(
                    javac_major(&second_jdk),
                    cold.java_major,
                    "{SECOND_JDK} must name a JDK of another feature release"
                );
                std::fs::remove_file(&home).expect("unlink the first JDK");
                std::os::unix::fs::symlink(&second_jdk, &home).expect("link the second JDK");
                let switched = run_jdk_probe(&home, &tag);
                assert_eq!(
                    (switched.hits, switched.misses, switched.java_major),
                    (0, 1, javac_major(&second_jdk)),
                    "the same case at the same JDK path recompiles under the replacement JDK"
                );
                assert_ne!(switched.key, cold.key);
                keys.push(switched.key);
            }
            None => eprintln!("set {SECOND_JDK} to check that another JDK misses the cache"),
        }

        for key in keys {
            std::fs::remove_dir_all(reference_cache_dir(key))
                .expect("remove the probe's inventory entry");
        }
        std::fs::remove_dir_all(&root).expect("remove the probe directory");
    }

    #[test]
    fn producing_jdk_probe() {
        let Some(tag) = std::env::var_os(JDK_PROBE) else {
            return;
        };
        let src = jdk_probe_source(tag.to_str().expect("UTF-8 probe tag"));
        let jdk = BoxJdk::Full { root: None };
        let classpath = [common::stdlib_jar()];
        let reference = reference_compile(&src, "box", &classpath, jdk, NO_HELPERS)
            .expect("the mixed-Java probe compiles");
        let key =
            reference_cache_key(&src, "box", &classpath, jdk, NO_HELPERS).expect("probe cache key");
        let java = reference
            .module(MAIN_MODULE)
            .and_then(|classes| classes.get("Greeting"))
            .expect("the javac-compiled class is in the inventory");
        let stats = reference_stats();
        println!(
            "jdk-probe key={key:032x} hits={} misses={} major={}",
            stats.hits,
            stats.misses,
            u16::from_be_bytes([java[6], java[7]])
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
