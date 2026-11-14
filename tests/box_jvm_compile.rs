//! How the box conformance lanes compile a corpus case with krusty, in process: the directive-
//! selected command line of each unit, the per-thread classpath reuse, and the case topologies
//! (single source, `// FILE:` sets with Java, and `// MODULE:` builds). The JVM box lane runs the
//! classes this emits; the diagnostics lane only asks whether a compilation reports errors.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::classreader::parse_class;

use krusty::conformance::split_modules;

use super::box_byte_score;
use super::box_reference_classes;
use super::common;

#[path = "box_jvm_compile/rejection_report.rs"]
mod rejection_report;

/// Time spent compiling single-source cases (ns, accumulated across all files and threads).
pub(super) static T_EMIT: AtomicU64 = AtomicU64::new(0);

/// Why krusty compiled a case to no classes.
#[derive(Debug)]
pub(super) enum CompileFailure {
    /// krusty reported errors: their messages, in the order it reported them, and the complete
    /// rendered report of every diagnostic.
    Rejected { errors: Vec<String>, report: String },
    /// The harness could not set up or finish the compilation: a command line krusty refuses, a
    /// `// MODULE:` shape it cannot split, javac refusing the case's Java, or a scratch write.
    Harness(String),
}

pub(super) type Compiled<T> = Result<T, CompileFailure>;

fn harness(reason: impl Into<String>) -> CompileFailure {
    CompileFailure::Harness(reason.into())
}

/// The classes one krusty compilation of `module` from `inputs` (named by `stems`, in the same
/// order) emitted, or the errors it reported.
fn emitted(
    outputs: Vec<(String, Vec<u8>)>,
    module: &str,
    inputs: &[krusty::source::SourceInput<'_>],
    stems: &[String],
    diags: &DiagSink,
) -> Compiled<Vec<(String, Vec<u8>)>> {
    if diags.has_errors() {
        return Err(CompileFailure::Rejected {
            errors: diags
                .diags
                .iter()
                .filter(|diagnostic| diagnostic.severity == krusty::diag::Severity::Error)
                .map(|diagnostic| diagnostic.msg.clone())
                .collect(),
            report: rejection_report::render(
                module,
                inputs
                    .iter()
                    .zip(stems)
                    .map(|(input, stem)| (input.kind, stem.as_str(), input.text)),
                diags,
            ),
        });
    }
    Ok(outputs
        .into_iter()
        .filter_map(|(path, bytes)| {
            path.strip_suffix(".class")
                .map(|internal| (internal.to_string(), bytes))
        })
        .collect())
}

thread_local! {
    /// Stable jar/jimage classpaths reused per rayon thread. Module and javac scratch directories are
    /// deliberately excluded: their unique paths would retain one complete `Classpath` per corpus case
    /// after the directory was deleted, eventually reducing signature collection to swap-bound work.
    static CP_CACHE: std::cell::RefCell<std::collections::HashMap<Vec<std::path::PathBuf>, std::rc::Rc<Classpath>>>
        = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn classpath_paths_are_cacheable(paths: &[PathBuf]) -> bool {
    paths.iter().all(|path| path.is_file())
}

fn harness_classpath(paths: Vec<PathBuf>) -> std::rc::Rc<Classpath> {
    if !classpath_paths_are_cacheable(&paths) {
        return std::rc::Rc::new(Classpath::new(paths));
    }
    CP_CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry(paths.clone())
            .or_insert_with(|| std::rc::Rc::new(Classpath::new(paths)))
            .clone()
    })
}

fn harness_classpath_with_friends(
    paths: Vec<PathBuf>,
    friend_paths: Vec<PathBuf>,
) -> std::rc::Rc<Classpath> {
    if friend_paths.is_empty() {
        harness_classpath(paths)
    } else {
        std::rc::Rc::new(Classpath::new_with_friend_paths(paths, friend_paths))
    }
}

/// In-memory dependency classes applied to the (usually thread-cached, SHARED) `Classpath` for one
/// compile, and ALWAYS cleared afterward — a stale overlay on the shared instance would leak one
/// case's classes into the next case on the same worker thread. Passing dependency classes this way
/// (instead of a scratch directory on the classpath) keeps the classpath entry set all-files, so
/// the per-thread instance — and its composed package tree — is reused instead of being rebuilt
/// per case, which profiling showed was ~a third of harness CPU.
struct OverlayGuard(std::rc::Rc<Classpath>);

impl OverlayGuard {
    fn set(cp: &std::rc::Rc<Classpath>, classes: &[(String, Vec<u8>)]) -> Option<OverlayGuard> {
        if classes.is_empty() {
            return None;
        }
        cp.set_stub_overlay(classes.to_vec());
        Some(OverlayGuard(cp.clone()))
    }
}

impl Drop for OverlayGuard {
    fn drop(&mut self) {
        self.0.clear_stub_overlay();
    }
}

/// The command line krusty compiles one unit of `case` under: the directive arguments the reference
/// compile of the same unit receives ([`krusty::conformance::unit_compiler_configuration`]).
/// Ordinary arguments pass through krusty's compatible CLI; exact levels additionally use the
/// harness's typed configuration channel, which also carries historical levels. `Err` carries every
/// refusal, in order.
pub(super) fn unit_options<'a>(
    case: &str,
    unit: impl IntoIterator<Item = &'a str>,
) -> Result<krusty_cli::cli::Options, String> {
    let configuration = krusty::conformance::unit_compiler_configuration(case, unit)?;
    let mut options = krusty_cli::cli::parse(configuration.arguments.clone());
    let refusals: Vec<&str> = options
        .argument_errors
        .iter()
        .chain(&options.errors)
        .map(String::as_str)
        .collect();
    if !refusals.is_empty() {
        return Err(refusals.join("\n"));
    }
    options.inline_scopes_numbers = configuration.inline_scopes_numbers;
    if configuration.language_version.is_some() || configuration.api_version.is_some() {
        let language_version = configuration
            .language_version
            .unwrap_or(options.language_settings.language_version);
        let api_version = configuration.api_version.or_else(|| {
            configuration
                .language_version
                .is_none()
                .then_some(options.language_settings.api_version)
        });
        // The command line already read the feature settings; only the levels change.
        let feature_settings = options.language_settings.feature_settings();
        options.override_language_versions(language_version, api_version, &feature_settings)?;
    }
    Ok(options)
}

/// What a unit analyzes against: its JVM libraries, with the native compiler plugins its command
/// line requests (`// WITH_COMPOSE`). A harness failure when it requests one krusty refuses.
fn plugin_provider(
    platform: Box<krusty::jvm::jvm_libraries::JvmLibraries>,
    options: &krusty_cli::cli::Options,
    classpath: &[std::path::PathBuf],
) -> Compiled<krusty::frontend::PlatformProvider> {
    let plugins = options.resolve_plugins(classpath);
    if !plugins.errors.is_empty() {
        return Err(harness(plugins.errors.join("\n")));
    }
    Ok(krusty::frontend::PlatformProvider::jvm(platform).with_native_plugins(plugins.native))
}

/// The JVM library provider a unit analyzes against, at the API version its command line selects.
fn jvm_libraries(
    cp: &std::rc::Rc<Classpath>,
    options: &krusty_cli::cli::Options,
) -> krusty::jvm::jvm_libraries::JvmLibraries {
    krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone())
        .expect("JVM provider initialization")
        .with_api_version(options.language_settings.api_version)
}

fn compile_source(
    src: &str,
    stem: &str,
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
    progress: &dyn Fn(&str),
) -> Compiled<Vec<(String, Vec<u8>)>> {
    let mut diags = DiagSink::new();
    let options = unit_options(src, [src]).map_err(harness)?;
    // The stdlib is on krusty's classpath only for `// WITH_STDLIB` tests — the caller passes the
    // located jar (or `None`), exactly as a drop-in `kotlinc` user supplies `-classpath`.
    // Explicit classpath: the kotlin-stdlib jar (for `// WITH_STDLIB`) plus the JDK `lib/modules`
    // jimage (the bootclasspath). The compiler never reads `JAVA_HOME` — the harness passes the
    // path, exactly as a `kotlinc -classpath` invocation would.
    let mut cp_paths: Vec<std::path::PathBuf> = cp_jars.to_vec();
    if let Some(p) = jdk_modules {
        cp_paths.push(p.to_path_buf());
    }
    // Reuse stable jar/jimage classpaths; scratch directory classpaths die with their compile.
    let provider = |platform| plugin_provider(platform, &options, &cp_paths);
    let cp = harness_classpath(cp_paths.clone());
    progress("two-pass FIR");
    let started = std::time::Instant::now();
    let platform = Box::new(jvm_libraries(&cp, &options));
    let inputs = [krusty::source::SourceInput::kotlin(src).with_file_stem(stem)];
    let stems = [stem.to_string()];
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        provider(platform)?,
        &options.language_settings.features,
        &mut diags,
    );
    let backend = options.jvm_backend(cp);
    let outputs = krusty::compiler::emit_analyzed(
        analysis,
        &stems,
        &backend,
        box_reference_classes::MAIN_MODULE,
        &mut diags,
    );
    T_EMIT.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    emitted(
        outputs,
        box_reference_classes::MAIN_MODULE,
        &inputs,
        &stems,
        &diags,
    )
}

/// The `helpers` package source the Kotlin test infra injects into every `// WITH_COROUTINES` box test
/// (kotlinc's `TestFiles.java` adds a `CoroutineUtil.kt` whose text is
/// `TestHelperGenerator.createTextForCoroutineHelpers(checkStateMachine, checkTailCallOptimization)`).
/// This is the `false, false` variant — the box corpus uses NEITHER `CHECK_STATE_MACHINE` nor
/// `CHECK_TAIL_CALL_OPTIMIZATION` (verified: 0 files), so the state-machine/tail-call checker classes are
/// never emitted. These helpers live in `kotlin.coroutines.*` (the stdlib), NOT `kotlinx-coroutines-core`
/// — no box test imports `kotlinx.coroutines`. Compiled as an extra source file in the same module so
/// `EmptyContinuation`, `runBlocking`, `handleResultContinuation`, … resolve exactly as under kotlinc.
pub(super) const COROUTINE_HELPERS: &str = krusty::conformance::COROUTINE_HELPERS;

/// Compile a `// FILE: name.kt`-split multi-file test as ONE module: parse each block, collect global
/// signatures, populate the cross-file function→facade map (`SymbolTable.fn_facades`, like the CLI
/// driver), then type-check + lower + emit each file, returning ALL classes. Returns `None` if any file
/// uses something the IR backend can't lower (e.g. a cross-file *class* reference — only cross-file
/// top-level functions are modeled so far), so the test SKIPS rather than miscompiles.
///
/// `// WITH_COROUTINES` tests are routed here too (even single-file): the generated `helpers` source is
/// appended as an extra block, mirroring kotlinc's `CoroutineUtil.kt` injection.
pub(super) fn compile_multifile(
    src: &str,
    main_stem: &str,
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Compiled<Vec<(String, Vec<u8>)>> {
    // Split on `// FILE: name.kt` markers (the preamble before the first marker is directives).
    // `.java` blocks are collected separately: javac compiles them first (in-process, via the
    // persistent JavaRunner) and their classes join krusty's classpath as an in-memory overlay,
    // mirroring how kotlinc's test infra makes Java sources visible to the Kotlin compile.
    let (mut blocks, java_blocks) = krusty::conformance::split_files(src).map_err(harness)?;
    // Single-file (no `// FILE:` markers) but routed here for coroutine-helper injection: the whole
    // source is the one main block.
    if blocks.is_empty() && java_blocks.is_empty() {
        blocks.push((main_stem.to_string(), src.to_string()));
    }
    // Mirror kotlinc: a `// WITH_COROUTINES` test gets the generated `helpers` source as an extra file.
    if krusty::conformance::directive(src, "WITH_COROUTINES") {
        let helpers =
            krusty::conformance::coroutine_sources(src, COROUTINE_HELPERS).map_err(harness)?;
        blocks.extend(helpers);
    }
    // A lone `// FILE: main.kt` block still names the file (and so its facade class), exactly as
    // kotlinc's test infra compiles it; only a source with no Kotlin at all is rejected here.
    if blocks.is_empty() {
        return Err(harness("the case has no Kotlin source"));
    }

    // Compile the Java blocks first (in-process javac, persistent JVM — no per-test spawn); krusty
    // sees the emitted classes as an overlay on the shared jar classpath, and they ride along to
    // BoxRunner's loader. When javac-first fails — the Java references Kotlin declarations, so it
    // cannot be ordered first — fall back to the KOTLIN-FIRST pipeline: signature stubs → krusty →
    // real javac.
    let mut java_classes: Vec<(String, Vec<u8>)> = Vec::new();
    if !java_blocks.is_empty() {
        match common::javac_compile(&java_blocks, cp_jars) {
            Some((javadir, classes)) => {
                // The classes were read into memory (BoxRunner loads from bytes, and krusty sees
                // them as an overlay on the shared jar classpath), so the scratch tree can go now.
                if let Some(root) = javadir.parent() {
                    let _ = fs::remove_dir_all(root);
                }
                java_classes = classes;
            }
            None => {
                let sources = UnitSources {
                    module: box_reference_classes::MAIN_MODULE,
                    kotlin: &blocks,
                    common: 0,
                    java: &java_blocks,
                };
                return compile_kotlin_first(src, sources, cp_jars, jdk_modules);
            }
        }
    }

    // Case-wide directives live in the preamble before the first `// FILE:`, so each unit's
    // arguments are read from the whole case.
    let compiled = compile_blocks(src, &blocks, cp_jars, &[], jdk_modules, None, &java_classes);
    let mut out = compiled?;
    out.extend(java_classes);
    Ok(out)
}

/// Kotlin-first mixed compilation for a test whose Java references Kotlin declarations. Kotlin and
/// Java files enter the production frontend together, so its JVM provider publishes the Java
/// declaration headers in Pass 1. Real javac runs only after Kotlin emission and only its classes
/// ship. Header installation or javac failure means the corpus case failed to compile.
fn compile_kotlin_first(
    src: &str,
    sources: UnitSources<'_>,
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Compiled<Vec<(String, Vec<u8>)>> {
    let java_blocks = sources.java;
    let kotlin_classes = compile_blocks_mixed(src, sources, cp_jars, &[], jdk_modules, None, &[])?;

    static UID: AtomicU64 = AtomicU64::new(0);
    let uid = UID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("krusty_ktfirst_{uid}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let result = (|| {
        // Real javac against krusty's output (the stubs are DISCARDED — javac must see the real
        // Kotlin classes so Java→Kotlin references type-check for real). javac reads files, so the
        // Kotlin classes are materialized, and any DIRECTORY entries of the caller's classpath
        // (module dependency dirs) stay on javac's classpath.
        let kotlindir = root.join("kotlin");
        write_classes_to_dir(&kotlin_classes, &kotlindir)?;
        let mut javac_cp = cp_jars.to_vec();
        javac_cp.push(kotlindir);
        let (javadir, java_classes) = common::javac_compile(java_blocks, &javac_cp)
            .ok_or_else(|| harness("javac rejected the Java sources against krusty's classes"))?;
        if let Some(jroot) = javadir.parent() {
            let _ = fs::remove_dir_all(jroot);
        }
        let mut out = kotlin_classes.clone();
        out.extend(java_classes);
        Ok(out)
    })();
    let _ = fs::remove_dir_all(&root);
    result
}

/// Compile a set of already-split source blocks `(stem, content)` of `case` as ONE krusty module
/// against the given classpath through the production two-pass FIR driver, returning all emitted
/// classes.
pub(super) fn compile_blocks(
    case: &str,
    blocks: &[(String, String)],
    cp_jars: &[std::path::PathBuf],
    friend_paths: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
    progress: Option<&dyn Fn(&str)>,
    overlay: &[(String, Vec<u8>)],
) -> Compiled<Vec<(String, Vec<u8>)>> {
    compile_blocks_mixed(
        case,
        UnitSources {
            module: box_reference_classes::MAIN_MODULE,
            kotlin: blocks,
            common: 0,
            java: &[],
        },
        cp_jars,
        friend_paths,
        jdk_modules,
        progress,
        overlay,
    )
}

/// One krusty compilation unit's sources. The first `common` Kotlin blocks are common (`dependsOn`)
/// sources, exactly as `krusty::conformance::module_units` orders a unit's chain.
#[derive(Clone, Copy)]
struct UnitSources<'a> {
    /// The unit's module name: kotlinc's `-module-name`, which the reference compile passes too. It
    /// names the `.kotlin_module` file, suffixes `internal` members and is recorded in `@Metadata`.
    module: &'a str,
    kotlin: &'a [(String, String)],
    common: usize,
    java: &'a [(String, String)],
}

/// Compile one unit of `case` under the command line its directives select ([`unit_options`]).
fn compile_blocks_mixed(
    case: &str,
    sources: UnitSources<'_>,
    cp_jars: &[std::path::PathBuf],
    friend_paths: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
    progress: Option<&dyn Fn(&str)>,
    overlay: &[(String, Vec<u8>)],
) -> Compiled<Vec<(String, Vec<u8>)>> {
    let UnitSources {
        module,
        kotlin: blocks,
        common,
        java: java_blocks,
    } = sources;
    let report = |phase: &str| {
        if let Some(progress) = progress {
            progress(phase);
        }
    };
    let mut options =
        unit_options(case, blocks.iter().map(|(_, source)| source.as_str())).map_err(harness)?;
    // A folded `dependsOn` chain is a multiplatform compilation even when the fixture does not
    // repeat the feature as a `// LANGUAGE:` directive. The common/platform source roles already
    // carry that topology into the frontend; enable the matching project feature on the exact
    // unit configuration instead of treating `expect`/`actual` as ordinary JVM source.
    if common != 0 {
        options
            .language_settings
            .features
            .enable("MultiPlatformProjects");
    }
    let mut diags = DiagSink::new();
    report("module classpath");
    let mut cp_paths: Vec<std::path::PathBuf> = cp_jars.to_vec();
    if let Some(p) = jdk_modules {
        cp_paths.push(p.to_path_buf());
    }
    let provider = |platform| plugin_provider(platform, &options, &cp_paths);
    let cp = harness_classpath_with_friends(cp_paths.clone(), friend_paths.to_vec());
    let _overlay = OverlayGuard::set(&cp, overlay);
    report("module two-pass FIR");
    let platform = Box::new(jvm_libraries(&cp, &options));
    let stems: Vec<String> = blocks
        .iter()
        .chain(java_blocks)
        .map(|(name, _)| name.clone())
        .collect();
    let mut inputs = blocks
        .iter()
        .enumerate()
        .map(|(index, (stem, content))| {
            let input = krusty::source::SourceInput::kotlin(content).with_file_stem(stem);
            if index < sources.common {
                input.common()
            } else {
                input
            }
        })
        .collect::<Vec<_>>();
    inputs.extend(
        java_blocks
            .iter()
            .map(|(stem, content)| krusty::source::SourceInput::java(content).with_file_stem(stem)),
    );
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        provider(platform)?,
        &options.language_settings.features,
        &mut diags,
    );
    let backend = options.jvm_backend(cp);
    let outputs = krusty::compiler::emit_analyzed(analysis, &stems, &backend, module, &mut diags);
    emitted(outputs, module, &inputs, &stems, &diags)
}

/// Write emitted `(internal_name, bytes)` classes under `dir` as `internal/name.class` (package dirs
/// created), so a later module's compile can read them off the classpath.
fn write_classes_to_dir(classes: &[(String, Vec<u8>)], dir: &Path) -> Compiled<()> {
    for (name, bytes) in classes {
        let path = dir.join(format!("{name}.class"));
        let written = match path.parent() {
            Some(parent) => fs::create_dir_all(parent).and_then(|()| fs::write(&path, bytes)),
            None => Ok(()),
        };
        written.map_err(|error| harness(format!("write {}: {error}", path.display())))?;
    }
    Ok(())
}

/// krusty's emitted classes for one box case: the flat list the JVM runner loads, plus the module
/// boundaries byte scoring pairs on. Every class is kept once in `flat`; `module_ranges` records
/// each module's contiguous half-open range into it, in emission order. A single-module topology
/// holds one range covering every class under [`box_reference_classes::MAIN_MODULE`]; a multi-module
/// build records one range per module so a class name shared by two modules stays two identities.
pub(super) struct CompiledClasses {
    pub(super) flat: Vec<(String, Vec<u8>)>,
    pub(super) module_ranges: Vec<(String, std::ops::Range<usize>)>,
}

impl CompiledClasses {
    /// Wrap a single module's flat class list (ordinary, `// FILE:`, or mixed-Java topology).
    pub(super) fn single_module(flat: Vec<(String, Vec<u8>)>) -> Self {
        let range = 0..flat.len();
        CompiledClasses {
            module_ranges: vec![(box_reference_classes::MAIN_MODULE.to_string(), range)],
            flat,
        }
    }

    /// Only the final canonical `ModuleUnit` owns the executable box entry, even if its
    /// range is empty. Dependencies remain in `flat` and their complete scoring ranges.
    pub(super) fn box_class(&self) -> Option<String> {
        let (_, range) = self.module_ranges.last()?;
        find_box_class(self.flat.get(range.clone())?)
    }

    /// The module-qualified inventory for byte scoring, keyed by `(module, internal name)`.
    pub(super) fn qualified(&self) -> box_byte_score::QualifiedClasses {
        let mut out = box_byte_score::QualifiedClasses::new();
        for (module, range) in &self.module_ranges {
            for (name, bytes) in &self.flat[range.clone()] {
                out.insert((module.clone(), name.clone()), bytes.clone());
            }
        }
        out
    }
}

/// Find the class that declares `static box()Ljava/lang/String;`.
pub(super) fn find_box_class(classes: &[(String, Vec<u8>)]) -> Option<String> {
    for (name, bytes) in classes {
        if let Ok(ci) = parse_class(bytes) {
            if ci
                .method("box", "()Ljava/lang/String;")
                .is_some_and(|m| m.is_static())
            {
                return Some(name.replace('/', "."));
            }
        }
    }
    None
}

/// Compile a `// MODULE:` test the way a Gradle multi-module build (or kotlinc's separate-compilation
/// test mode) does — but with EVERY module compiled by KRUSTY, so this also exercises krusty's own
/// `@Metadata` WRITE→READ round-trip across a real classpath boundary. Each module is compiled in
/// declaration (dependency) order against its dependency modules' EMITTED classes on the classpath; all
/// modules' classes are returned together so the persistent BoxRunner loads them in one classloader and
/// runs `box()`. `None` (SKIP) if the shape is unsupported or any module fails to lower.
pub(super) fn compile_module_test(
    src: &str,
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
    progress: &dyn Fn(&str),
) -> Compiled<CompiledClasses> {
    static UID: AtomicU64 = AtomicU64::new(0);
    let mut modules = split_modules(src).ok_or_else(|| harness("unsupported // MODULE: shape"))?;
    // kotlinc's `// WITH_COROUTINES` helpers live in an implicit `support` module every module sees.
    if krusty::conformance::directive(src, "WITH_COROUTINES") {
        krusty::conformance::inject_coroutine_support(&mut modules, src, COROUTINE_HELPERS)
            .map_err(harness)?;
    }
    let uid = UID.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("krusty_modtest_{}_{uid}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let compiled = compile_module_units(src, &modules, cp_jars, jdk_modules, progress, &tmp);
    // The dependency classes were read off disk during compilation and are now all held in memory
    // (for the in-memory BoxRunner), so the scratch dir is no longer needed.
    let _ = fs::remove_dir_all(&tmp);
    compiled
}

/// Compile each unit of a `// MODULE:` build in dependency order, writing each unit's classes
/// under `tmp` for its dependents' classpath.
fn compile_module_units(
    src: &str,
    modules: &[krusty::conformance::ModuleBlock],
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
    progress: &dyn Fn(&str),
    tmp: &Path,
) -> Compiled<CompiledClasses> {
    let mut dirmap: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    let mut all: Vec<(String, Vec<u8>)> = Vec::new();
    let mut module_ranges: Vec<(String, std::ops::Range<usize>)> = Vec::new();
    // Build units: a `dependsOn` target compiles INTO its dependents (kotlinc's JVM MPP model — the
    // platform module and its dependsOn chain are ONE compilation), so each unit's sources already
    // carry the chain dependency-first.
    for m in &krusty::conformance::module_units(modules) {
        // Compile-time classpath = the base (stdlib/JDK) + each dependency module's emitted-class
        // dir. FRIEND deps ride the same classpath (their `internal` visibility is the friend
        // part; krusty resolves them like any dependency).
        let mut cp = cp_jars.to_vec();
        let mut friend_paths = Vec::new();
        for d in &m.deps {
            // A dependency declared out of order, or on an unbuilt module.
            let p = dirmap.get(d).ok_or_else(|| {
                harness(format!("module {}: dependency {d} was not built", m.name))
            })?;
            cp.push(p.clone());
            if m.friends.iter().any(|friend| friend == d) {
                friend_paths.push(p.clone());
            }
        }
        let (files, java_files) = (&m.files, &m.java_files);
        // The unit's `dependsOn` chain sources come first and are common sources, as in kotlinc's
        // `-Xcommon-sources` compilation of one platform module.
        let kotlin_only = UnitSources {
            module: &m.name,
            kotlin: files,
            common: m.common_file_count,
            java: &[],
        };
        let report = |phase: &str| progress(&format!("module {}: {phase}", m.name));
        let moddir = tmp.join(&m.name);
        // A source-less unit (an empty hmpp intermediate built standalone) emits nothing; it still
        // gets a (created, empty) classpath dir so dependents resolve it.
        if files.is_empty() && java_files.is_empty() {
            fs::create_dir_all(&moddir)
                .map_err(|error| harness(format!("create {}: {error}", moddir.display())))?;
            dirmap.insert(m.name.clone(), moddir);
            continue;
        }
        // A module's `.java` sources: javac-first against the module classpath (Java referencing
        // only deps/JDK); when that fails — the Java references THIS module's Kotlin — fall back to
        // the Kotlin-first stub pipeline, exactly like the single-module path.
        let classes = if java_files.is_empty() {
            compile_blocks_mixed(
                src,
                kotlin_only,
                &cp,
                &friend_paths,
                jdk_modules,
                Some(&report),
                &[],
            )?
        } else {
            match common::javac_compile(java_files, &cp) {
                Some((javadir, java_classes)) => {
                    if let Some(root) = javadir.parent() {
                        let _ = fs::remove_dir_all(root);
                    }
                    // A Java-only module has no Kotlin to compile.
                    let mut kotlin = if files.is_empty() {
                        Vec::new()
                    } else {
                        // The javac classes ride the overlay (Java classes carry no Kotlin
                        // facades, so the class-lookup channels the overlay feeds suffice). The
                        // Kotlin dependency DIRS stay on the classpath: dependency-module
                        // top-level functions resolve through the package catalog, which only
                        // directory/jar entries contribute to — an overlaid dependency module
                        // loses them (measured: -95 box passes).
                        compile_blocks_mixed(
                            src,
                            kotlin_only,
                            &cp,
                            &friend_paths,
                            jdk_modules,
                            None,
                            &java_classes,
                        )?
                    };
                    kotlin.extend(java_classes);
                    kotlin
                }
                None => compile_kotlin_first(
                    src,
                    UnitSources {
                        java: java_files,
                        ..kotlin_only
                    },
                    &cp,
                    jdk_modules,
                )?,
            }
        };
        write_classes_to_dir(&classes, &moddir)?;
        dirmap.insert(m.name.clone(), moddir);
        let start = all.len();
        all.extend(classes);
        // Byte scoring pairs by module identity, so each module's classes stay a distinct range
        // even when two modules emit the same internal class name.
        module_ranges.push((m.name.clone(), start..all.len()));
    }
    Ok(CompiledClasses {
        flat: all,
        module_ranges,
    })
}

/// Compile one box case with krusty through the topology its markers select: `// MODULE:` builds,
/// then `// FILE:` multi-file sets (also used for `// WITH_COROUTINES`, which needs the generated
/// `helpers` source compiled alongside it), then a single source file.
pub(super) fn compile_box_case(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&Path>,
    progress: &dyn Fn(&str),
) -> Compiled<CompiledClasses> {
    if src.contains("// MODULE:") {
        compile_module_test(src, cp_jars, jdk_modules, progress)
    } else if src.contains("// FILE:") || src.contains("// WITH_COROUTINES") {
        compile_multifile(src, stem, cp_jars, jdk_modules).map(CompiledClasses::single_module)
    } else {
        compile_source(src, stem, cp_jars, jdk_modules, progress)
            .map(CompiledClasses::single_module)
    }
}

#[test]
fn scratch_directory_classpaths_are_not_retained() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert!(!classpath_paths_are_cacheable(std::slice::from_ref(
        &manifest
    )));
    assert!(classpath_paths_are_cacheable(
        &[manifest.join("Cargo.toml")]
    ));
}
