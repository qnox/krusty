//! The `codegen/box` corpus through a backend that builds a RUNNABLE program.
//!
//! The JVM lane compares class files and calls `box()` in a JVM it already has. Every other target
//! — Native, `wasm-js`, `wasm-wasi` — instead compiles a case into a program whose entry prints
//! `box()`'s answer after [`BOX_RESULT_FRAME`], runs it, and reads the answer back. What differs
//! between those targets is only how a case is compiled and started, which a [`Lane`] supplies;
//! applicability, the run with its deadline and bounded output, the verdict, the committed
//! expected-failure ratchet and the report are this file's, and are the same for all of them.
//!
//! **Skipping is recorded; miscompiling is not silently accepted.** The generic box ratchet keeps
//! committed expected-failure and not-applicable inventories for `(platform, exact Kotlin
//! version)`. A new non-pass, a stale entry, or any transition between pass/fail/not-applicable
//! fails until the reviewed inventory is updated; compiler panics are never expected.
//!
//! The harness joins `KRUSTY_KOTLIN_BOX_DIR` with owned portable regressions in one inventory.
//! Every sweep requires its provisioned corpus and lane; the separate vendored smoke test does
//! not stand in for missing sweep setup. Owned fixtures require a JVM oracle before compilation,
//! and their harness errors, panics and applicability exclusions can never be blessed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use krusty::backend::{Artifact, Backend, BOX_RESULT_FRAME};
use krusty::conformance::TestTarget;
use krusty::diag::{DiagSink, Severity};
use krusty::jvm::classpath::Classpath;
use krusty::source::SourceInput;

use super::box_ratchet::{self, Platform};

/// How long one case may run. The programs are tiny; anything near this is a miscompiled loop.
const RUN_TIMEOUT: Duration = Duration::from_secs(10);

/// Retain enough trailing output for the framed verdict and diagnostics while continuously draining
/// both child pipes. A broken program may print without bound until the deadline; the harness must
/// neither deadlock nor turn that into unbounded memory/disk growth.
const OUTPUT_TAIL_LIMIT: usize = 16 * 1024 * 1024;

/// What happened to one case.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `box()` printed `OK`.
    Pass,
    /// The backend declined a construct, named.
    Declined(String),
    /// The frontend rejected the source (not this backend's business), carrying what it said.
    ///
    /// The reason is kept for the same purpose a decline's is: this bucket is a thousand cases
    /// wide and opaque without it, and what is in it is core work that every backend would gain
    /// from — so it is ranked in the report rather than left as a number.
    Frontend(String),
    /// The compiler panicked. A compiler defect always fails the lane, independent of phase.
    CompilerPanic(String),
    /// The corpus case applies to the target, but this runner cannot yet construct its
    /// compilation. This is a failure in the committed ratchet, never a denominator exclusion.
    Harness(String),
    /// A target directive or source universe puts the case outside this target lane.
    NotApplicable(&'static str),
    /// Accepted, but the program did not run correctly.
    Failed(String),
}

/// The box lanes may compile sources that legitimately emit warnings. Only an error means the
/// frontend rejected a case; retaining this distinction lets warning-bearing programs continue to
/// target emission and execution just as an ordinary compiler invocation does.
fn first_error(diags: &DiagSink, start: usize) -> Option<&krusty::diag::Diagnostic> {
    diags
        .diags
        .iter()
        .skip(start)
        .find(|diagnostic| diagnostic.severity == Severity::Error)
}

/// One target's way of compiling and starting a case.
pub trait Lane: Sync {
    fn platform(&self) -> Platform;

    /// How the report and failures name the lane (`Native`, `wasm-js`).
    fn label(&self) -> &'static str;

    /// The infix of the lane's environment knobs: `KRUSTY_<infix>_BOX_ONLY`,
    /// `KRUSTY_<infix>_BOX_LIMIT`, `KRUSTY_<infix>_BOX_TRACE`, `KRUSTY_<infix>_CONFORMANCE_REPORT`
    /// and `KRUSTY_REQUIRE_<infix>_CONFORMANCE`.
    fn env_infix(&self) -> &'static str;

    fn test_target(&self) -> TestTarget;

    /// What this lane's cases are analyzed against.
    fn libraries(&self) -> Libraries;

    /// Why this host cannot run the lane, or `None` when it can.
    fn unavailable(&self) -> Option<String>;

    /// Why a case is outside this target before any compilation, or `None` when it applies.
    fn not_applicable(&self, src: &str) -> Option<&'static str>;

    /// Why an applicable case cannot yet be constructed by this runner.
    fn harness_limitation(&self, src: &str) -> Option<&'static str>;

    /// Compile `sources` and run the program; `stem` names the case for scratch files.
    fn compile_and_run(
        &self,
        scratch: &Path,
        stem: &str,
        sources: &[(String, String)],
        directive_source: &str,
    ) -> Outcome;

    /// Compile independently analyzed source modules and run the resulting target program.
    fn compile_modules_and_run(
        &self,
        scratch: &Path,
        stem: &str,
        modules: &[krusty::conformance::ModuleUnit],
        directive_source: &str,
    ) -> Outcome;
}

pub fn ratchet_outcome(outcome: &Outcome) -> box_ratchet::Outcome {
    match outcome {
        Outcome::Pass => box_ratchet::Outcome::Pass,
        Outcome::NotApplicable(_) => box_ratchet::Outcome::NotApplicable,
        Outcome::Declined(_)
        | Outcome::Frontend(_)
        | Outcome::CompilerPanic(_)
        | Outcome::Harness(_)
        | Outcome::Failed(_) => box_ratchet::Outcome::Fail,
    }
}

fn validate_bless(bless: bool, full_run: bool, provisioned: bool, ci: bool) -> Result<(), String> {
    if !bless {
        return Ok(());
    }
    if !full_run || !provisioned {
        return Err(
            "KRUSTY_BLESS_BOX_EXPECTATIONS needs a full run over the provisioned corpus".into(),
        );
    }
    if ci {
        return Err(
            "KRUSTY_BLESS_BOX_EXPECTATIONS is refused under CI; review and update the manifests from a deliberate local run".into(),
        );
    }
    Ok(())
}

pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(lane: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-{lane}-box-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What a lane's cases are analyzed against. It is the lane's own setting, so a target moves to its
/// own library surface without forking the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Libraries {
    /// The JVM stdlib and `kotlin-test` jars with the JDK modules, which is what the JVM lane
    /// compiles against. A mapped builtin like `kotlin.Throwable` is a typealias for a `java.lang`
    /// class there, so the JDK has to be present for those names to resolve at all.
    JvmArtifacts,
    /// The target's own stdlib KLIB (`toolchain::kotlin_stdlib_klib`), the library kotlinc
    /// compiles that target against. Native includes `kotlin.test`; Wasm adds its exact target
    /// kotlin-test KLIB as an ordinary dependency.
    StdlibKlib,
}

struct PlatformLibraries {
    semantics: Box<dyn krusty::libraries::SemanticPlatform>,
    native_imports: Vec<krusty::native::ImportLibrary>,
}

/// Target artifacts and the frozen Native library facts selected by the same compilation inputs.
pub struct CompiledProgram {
    pub artifacts: Vec<Artifact>,
    pub native_imports: Vec<krusty::native::ImportLibrary>,
}

impl Libraries {
    /// Whether this host has the libraries a `target` case is analyzed against.
    pub fn available(self, target: TestTarget) -> bool {
        match self {
            Self::JvmArtifacts => {
                krusty::toolchain::stdlib_jar().is_some()
                    && krusty::toolchain::jdk_modules().is_some()
            }
            Self::StdlibKlib => {
                krusty::toolchain::kotlin_stdlib_klib(target.compilation_target()).is_some()
            }
        }
    }

    /// A fresh semantic platform for one compilation.
    fn platform(
        self,
        target: krusty::compilation_target::CompilationTarget,
        dependencies: &[PathBuf],
        friend_dependencies: &[PathBuf],
    ) -> PlatformLibraries {
        match self {
            Self::JvmArtifacts if !dependencies.is_empty() || !friend_dependencies.is_empty() => {
                panic!("JVM box libraries cannot ingest KLIB dependency modules")
            }
            Self::JvmArtifacts => PlatformLibraries {
                semantics: Box::new(
                    krusty::jvm::jvm_libraries::JvmLibraries::new(
                        CLASSPATH.with(std::rc::Rc::clone),
                    )
                    .expect("JVM provider initialization"),
                ),
                native_imports: Vec::new(),
            },
            Self::StdlibKlib => STDLIB_KLIBS.with(|opened| {
                if !dependencies.is_empty() {
                    let mut libraries = dependencies
                        .iter()
                        .map(|path| {
                            if friend_dependencies.contains(path) {
                                krusty::klib_libraries::KlibLibraryInput::friend(path.clone())
                            } else {
                                krusty::klib_libraries::KlibLibraryInput::dependency(path.clone())
                            }
                        })
                        .collect::<Vec<_>>();
                    libraries.push(krusty::klib_libraries::KlibLibraryInput::platform_stdlib(
                        krusty::toolchain::kotlin_stdlib_klib(target)
                            .expect("checked by `Libraries::available`"),
                        klib_stdlib_semantics(target),
                    ));
                    libraries.extend(wasm_test_library(target));
                    let (libraries, native_imports) = open_klib_inputs(target, &libraries)
                        .unwrap_or_else(|error| {
                            panic!("opening the {target:?} dependency KLIBs: {error}")
                        });
                    return PlatformLibraries {
                        semantics: Box::new(libraries),
                        native_imports,
                    };
                }
                let mut opened = opened.borrow_mut();
                if let Some((_, libraries, native_imports)) =
                    opened.iter().find(|(opened, _, _)| *opened == target)
                {
                    return PlatformLibraries {
                        semantics: Box::new(libraries.for_compilation()),
                        native_imports: native_imports.clone(),
                    };
                }
                let stdlib = krusty::toolchain::kotlin_stdlib_klib(target)
                    .expect("checked by `Libraries::available`");
                let mut inputs = vec![krusty::klib_libraries::KlibLibraryInput::platform_stdlib(
                    stdlib,
                    klib_stdlib_semantics(target),
                )];
                inputs.extend(wasm_test_library(target));
                let (libraries, native_imports) = open_klib_inputs(target, &inputs)
                    .unwrap_or_else(|error| panic!("opening the {target:?} stdlib KLIB: {error}"));
                let compilation = PlatformLibraries {
                    semantics: Box::new(libraries.for_compilation()),
                    native_imports: native_imports.clone(),
                };
                opened.push((target, libraries, native_imports));
                compilation
            }),
        }
    }
}

/// Wasm publishes kotlin-test separately from its stdlib. Retain it as an ordinary dependency:
/// these coordinates select the exact target/version but confer no compiler intrinsic authority.
fn wasm_test_library(
    target: krusty::compilation_target::CompilationTarget,
) -> Option<krusty::klib_libraries::KlibLibraryInput> {
    use krusty::compilation_target::CompilationTarget;
    let artifact = match target {
        CompilationTarget::WasmJs => "kotlin-test-wasm-js",
        CompilationTarget::WasmWasi => "kotlin-test-wasm-wasi",
        _ => return None,
    };
    let version = krusty::kotlin_version::target().to_string();
    let path = krusty::toolchain::ensure_maven_artifact(
        "org.jetbrains.kotlin",
        artifact,
        &version,
        "klib",
    )
    .unwrap_or_else(|| panic!("cannot provision exact {artifact}:{version}"));
    Some(krusty::klib_libraries::KlibLibraryInput::dependency(path))
}

fn klib_stdlib_semantics(
    target: krusty::compilation_target::CompilationTarget,
) -> krusty::klib_libraries::KlibStdlibSemantics {
    use krusty::compilation_target::CompilationTarget;
    use krusty::klib_libraries::KlibStdlibSemantics;

    match target {
        CompilationTarget::Native => KlibStdlibSemantics::Native,
        CompilationTarget::WasmJs | CompilationTarget::WasmWasi => KlibStdlibSemantics::Wasm,
        CompilationTarget::Js => KlibStdlibSemantics::Plain,
        CompilationTarget::Jvm => panic!("JVM box libraries cannot ingest a KLIB stdlib"),
    }
}

fn open_klib_inputs(
    target: krusty::compilation_target::CompilationTarget,
    inputs: &[krusty::klib_libraries::KlibLibraryInput],
) -> Result<
    (
        krusty::klib_libraries::KlibLibraries,
        Vec<krusty::native::ImportLibrary>,
    ),
    String,
> {
    if target == krusty::compilation_target::CompilationTarget::Native {
        let native_target = krusty::native::NativeTarget::host()
            .ok_or_else(|| "the host has no Native target".to_string())?;
        return krusty::native::NativeKlibInputs::open(inputs, native_target)
            .map(krusty::native::NativeKlibInputs::into_parts)
            .map_err(|error| error.to_string());
    }
    krusty::klib_libraries::KlibLibraries::open(inputs)
        .map(|libraries| (libraries, Vec::new()))
        .map_err(|error| error.to_string())
}

thread_local! {
    /// One classpath per worker thread: it is `Rc`-shared and caches parsed class files, so
    /// building it per case would redo the stdlib jar's index thousands of times.
    static CLASSPATH: std::rc::Rc<Classpath> = std::rc::Rc::new(Classpath::new(
        [
            krusty::toolchain::stdlib_jar().expect("checked by `Libraries::available`"),
            krusty::toolchain::jdk_modules().expect("checked by `Libraries::available`"),
        ]
        .into_iter()
        // `kotlin-test`, which is what `// WITH_STDLIB` means on top of the stdlib: the corpus
        // checks itself with `assertEquals`, `assertTrue` and `assertFailsWith`, and without the
        // jar those do not resolve — nor does the `kotlin.test` package path itself.
        .chain(krusty::toolchain::kotlin_test_jar())
        .collect::<Vec<_>>(),
    ));

    /// Each target's stdlib KLIB, opened once per worker thread: opening decodes and signs every
    /// declaration, which `for_compilation` then shares with each case.
    static STDLIB_KLIBS: std::cell::RefCell<
        Vec<(
            krusty::compilation_target::CompilationTarget,
            krusty::klib_libraries::KlibLibraries,
            Vec<krusty::native::ImportLibrary>,
        )>,
    > = const { std::cell::RefCell::new(Vec::new()) };
}

thread_local! {
    /// The file the most recent panic on this thread was raised in, recorded by the panic hook so
    /// a caught panic can be attributed to a backend or to the rest of the compiler.
    static LAST_PANIC_FILE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Analyze `sources` for `target` and emit them with `backend`; every artifact, or why not.
/// A diagnostic beginning with `decline_prefix` is the backend declining a construct.
pub fn compile<B: Backend>(
    sources: &[(String, String)],
    directive_source: &str,
    libraries: Libraries,
    target: TestTarget,
    backend: &B,
    module_name: &str,
    decline_prefix: &str,
) -> Result<CompiledProgram, Outcome> {
    let inputs = sources
        .iter()
        .map(|(stem, source)| SourceInput::kotlin(source).with_file_stem(stem))
        .collect::<Vec<_>>();
    let stems = sources
        .iter()
        .map(|(stem, _)| stem.clone())
        .collect::<Vec<_>>();
    let compilation_target = backend.compilation_target();
    if compilation_target != target.compilation_target() {
        return Err(Outcome::Harness(format!(
            "the {target:?} corpus lane received a {compilation_target:?} backend"
        )));
    }
    let PlatformLibraries {
        semantics,
        native_imports,
    } = libraries.platform(compilation_target, &[], &[]);
    let platform = krusty::frontend::PlatformProvider::new(compilation_target, semantics);
    let features = krusty::features::LangFeatures::from_source(directive_source);
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    if let Some(first) = first_error(&diags, 0) {
        return Err(Outcome::Frontend(first.msg.clone()));
    }
    let artifacts =
        krusty::compiler::emit_analyzed(analysis, &stems, backend, module_name, &mut diags);
    if let Some(decline) = diags
        .diags
        .iter()
        .find_map(|diagnostic| diagnostic.msg.strip_prefix(decline_prefix))
    {
        return Err(Outcome::Declined(
            decline.trim_end_matches(" yet").to_string(),
        ));
    }
    if let Some(first) = first_error(&diags, 0) {
        return Err(Outcome::Frontend(first.msg.clone()));
    }
    Ok(CompiledProgram {
        artifacts,
        native_imports,
    })
}

/// Analyze one independently built corpus module against the exact KLIB artifacts of its
/// dependencies. `common_file_count` is the dependency-first `dependsOn` prefix folded into this
/// unit; those sources remain common rather than being flattened into platform sources.
fn module_features(
    directive_source: &str,
    common_file_count: usize,
) -> krusty::features::LangFeatures {
    let mut features = krusty::features::LangFeatures::from_source(directive_source);
    if common_file_count != 0 {
        features.enable("MultiPlatformProjects");
    }
    features
}

pub fn analyze_module(
    unit: &krusty::conformance::ModuleUnit,
    dependency_klibs: &[PathBuf],
    friend_klibs: &[PathBuf],
    directive_source: &str,
    libraries: Libraries,
    target: TestTarget,
    backend_target: krusty::compilation_target::CompilationTarget,
    diags: &mut DiagSink,
) -> Result<
    (
        krusty::frontend::StreamingSourceSetAnalysis,
        Vec<String>,
        Vec<krusty::native::ImportLibrary>,
    ),
    Outcome,
> {
    if backend_target != target.compilation_target() {
        return Err(Outcome::Harness(format!(
            "the {target:?} corpus lane received a {backend_target:?} backend"
        )));
    }
    let inputs = unit
        .files
        .iter()
        .enumerate()
        .map(|(index, (stem, source))| {
            let input = SourceInput::kotlin(source).with_file_stem(stem);
            if index < unit.common_file_count {
                input.common()
            } else {
                input
            }
        })
        .collect::<Vec<_>>();
    let stems = unit
        .files
        .iter()
        .map(|(stem, _)| stem.clone())
        .collect::<Vec<_>>();
    let PlatformLibraries {
        semantics,
        native_imports,
    } = libraries.platform(backend_target, dependency_klibs, friend_klibs);
    let platform = krusty::frontend::PlatformProvider::new(backend_target, semantics);
    let features = module_features(directive_source, unit.common_file_count);
    let diagnostics_start = diags.diags.len();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_module(
        &inputs, platform, &features, &unit.name, diags,
    );
    if let Some(first) = first_error(diags, diagnostics_start) {
        return Err(Outcome::Frontend(first.msg.clone()));
    }
    Ok((analysis, stems, native_imports))
}

/// Analyze each source module against only its declared dependency KLIB closure, emit all target
/// code through one backend session, and retain every generated dependency as a distinct archive.
/// The target lane supplies `metadata`: Native owns its concrete Konan target here, while each Wasm
/// lane owns its exact JS/WASI platform.
pub fn compile_modules<B: Backend, M: krusty::backend::DependencyMetadata>(
    scratch: &Path,
    stem: &str,
    modules: &[krusty::conformance::ModuleUnit],
    directive_source: &str,
    libraries: Libraries,
    target: TestTarget,
    backend: &B,
    metadata: &M,
    decline_prefix: &str,
) -> Result<CompiledProgram, Outcome> {
    #[derive(Clone)]
    struct GeneratedDependency {
        archive: PathBuf,
        transitive: Vec<PathBuf>,
    }

    struct GeneratedRoot(PathBuf);

    impl Drop for GeneratedRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn diagnostic_outcome(diags: &DiagSink, decline_prefix: &str) -> Option<Outcome> {
        if let Some(decline) = diags
            .diags
            .iter()
            .find_map(|diagnostic| diagnostic.msg.strip_prefix(decline_prefix))
        {
            return Some(Outcome::Declined(
                decline.trim_end_matches(" yet").to_string(),
            ));
        }
        first_error(diags, 0).map(|first| Outcome::Frontend(first.msg.clone()))
    }

    fn write_dependency_archive(
        root: &Path,
        ordinal: usize,
        artifacts: &[Artifact],
    ) -> Result<PathBuf, Outcome> {
        let archive = root.join(format!("module-{ordinal}"));
        for (name, bytes) in artifacts {
            let relative = Path::new(name);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(Outcome::Harness(format!(
                    "generated KLIB contains an unsafe artifact path {name:?}"
                )));
            }
            let output = archive.join(relative);
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    Outcome::Failed(format!("creating generated KLIB directory: {error}"))
                })?;
            }
            std::fs::write(&output, bytes).map_err(|error| {
                Outcome::Failed(format!("writing generated KLIB artifact {name}: {error}"))
            })?;
        }
        Ok(archive)
    }

    let compilation_target = backend.compilation_target();
    if compilation_target != target.compilation_target() {
        return Err(Outcome::Harness(format!(
            "the {target:?} corpus lane received a {compilation_target:?} backend"
        )));
    }
    if metadata.compilation_target() != compilation_target {
        return Err(Outcome::Harness(format!(
            "the {target:?} corpus lane received {:?} dependency metadata",
            metadata.compilation_target()
        )));
    }
    let generated_root = GeneratedRoot(scratch.join(format!(
        ".krusty-klib-{stem}-{}-{}",
        std::process::id(),
        rayon::current_thread_index().unwrap_or(0)
    )));
    if let Err(error) = std::fs::create_dir_all(&generated_root.0) {
        return Err(Outcome::Failed(format!(
            "creating generated KLIB root: {error}"
        )));
    }

    let mut session = krusty::compiler::EmissionSession::new(backend);
    let mut diags = DiagSink::new();
    let mut generated = BTreeMap::<String, GeneratedDependency>::new();
    let mut native_imports = Vec::new();
    for (ordinal, unit) in modules.iter().enumerate() {
        let mut dependency_klibs = Vec::new();
        let mut friend_klibs = Vec::new();
        let mut seen = BTreeSet::new();
        for dependency in &unit.deps {
            let Some(archive) = generated.get(dependency) else {
                return Err(Outcome::Harness(format!(
                    "module dependency {dependency} was not built"
                )));
            };
            for path in std::iter::once(&archive.archive).chain(&archive.transitive) {
                if seen.insert(path.clone()) {
                    dependency_klibs.push(path.clone());
                }
            }
            if unit.friends.contains(dependency) {
                friend_klibs.push(archive.archive.clone());
            }
        }
        let (analysis, stems, module_imports) = analyze_module(
            unit,
            &dependency_klibs,
            &friend_klibs,
            directive_source,
            libraries,
            target,
            compilation_target,
            &mut diags,
        )?;
        native_imports.extend(module_imports);
        let Some(dependency) =
            session.emit_module(analysis, &stems, metadata, &unit.name, &mut diags)
        else {
            let outcome = diagnostic_outcome(&diags, decline_prefix).unwrap_or_else(|| {
                Outcome::Harness("module emission failed without a diagnostic".to_string())
            });
            return Err(outcome);
        };
        let archive = write_dependency_archive(&generated_root.0, ordinal, dependency.artifacts())?;
        generated.insert(
            unit.name.clone(),
            GeneratedDependency {
                archive,
                transitive: dependency_klibs,
            },
        );
    }
    let artifacts = session.finish("box", &mut diags);
    let outcome = diagnostic_outcome(&diags, decline_prefix);
    if let Some(outcome) = outcome {
        return Err(outcome);
    }
    Ok(CompiledProgram {
        artifacts,
        native_imports,
    })
}

/// Run a program with the lane's deadline; its framed verdict.
pub fn run_program(command: &mut Command) -> Outcome {
    match run_with_timeout(command, RUN_TIMEOUT) {
        // The entry prints what `box()` returned after `BOX_RESULT_FRAME`, and nothing after it:
        // the answer is every byte past the one frame. Anything before it is the PROGRAM's own
        // output — `controlStructures/kt513.kt` builds a list and `println`s it before returning
        // `OK` — which the JVM lane never sees, because there the answer is a return value rather
        // than a stream. The answer is compared whole, so one that spans lines (`"FAIL\nOK"`) is
        // the wrong answer it is, not a last line that happens to read OK.
        Ok(output) => framed_outcome(output),
        Err(ProgramRunError::Harness(error)) => Outcome::Harness(error),
        Err(ProgramRunError::Program(error)) => Outcome::Failed(error),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ProgramRunError {
    Harness(String),
    Program(String),
}

/// Run the program with a deadline; its stdout, or what went wrong.
///
/// Both pipes are drained concurrently into bounded trailing buffers. Waiting before draining can
/// deadlock once either pipe fills; retaining output without a bound lets a broken program fill the
/// host until the deadline. Keeping the tail preserves the final result frame.
fn run_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<ProgramOutput, ProgramRunError> {
    let mut child = super::common::spawn_freshly_written(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .map_err(|error| ProgramRunError::Harness(format!("could not start the program: {error}")))?;
    let stdout = child.stdout.take().expect("piped child stdout");
    let stderr = child.stderr.take().expect("piped child stderr");
    let stdout = std::thread::spawn(move || output_tail(stdout));
    let stderr = std::thread::spawn(move || output_tail(stderr));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout.join();
                let _ = stderr.join();
                return Err(ProgramRunError::Program(format!(
                    "the program did not finish within {}s",
                    timeout.as_secs()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout.join();
                let _ = stderr.join();
                return Err(ProgramRunError::Harness(format!(
                    "waiting for the program: {error}"
                )));
            }
        }
    };
    let stdout = captured_output(stdout, "stdout").map_err(ProgramRunError::Harness)?;
    let stderr = captured_output(stderr, "stderr").map_err(ProgramRunError::Harness)?;
    let stdout_text = String::from_utf8_lossy(&stdout.bytes).into_owned();
    if !status.success() {
        return Err(ProgramRunError::Program(format!(
            "exit {}; stdout{} {stdout_text:?}; stderr{} {:?}",
            status,
            if stdout.truncated {
                " (truncated to tail)"
            } else {
                ""
            },
            if stderr.truncated {
                " (truncated to tail)"
            } else {
                ""
            },
            String::from_utf8_lossy(&stderr.bytes).trim()
        )));
    }
    Ok(ProgramOutput {
        stdout: stdout_text,
        result_frames: stdout.result_frames,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct ProgramOutput {
    stdout: String,
    /// Counted while draining, before the bounded tail can discard an earlier marker.
    result_frames: usize,
}

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
    result_frames: usize,
}

fn output_tail(mut input: impl Read) -> std::io::Result<CapturedOutput> {
    output_tail_with_limit(&mut input, OUTPUT_TAIL_LIMIT)
}

fn output_tail_with_limit(mut input: impl Read, limit: usize) -> std::io::Result<CapturedOutput> {
    assert!(limit > 0, "output tail limit must be positive");
    let mut tail = VecDeque::with_capacity(limit);
    let marker = BOX_RESULT_FRAME.as_bytes();
    let mut marker_prefix = Vec::with_capacity(marker.len().saturating_sub(1));
    let mut result_frames = 0;
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        let read = input.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        // Count over the complete stream while retaining only a bounded diagnostic tail. Keep the
        // preceding marker-length prefix so a frame split across reads is counted exactly once.
        let prefix_len = marker_prefix.len();
        marker_prefix.extend_from_slice(&chunk[..read]);
        result_frames += marker_prefix
            .windows(marker.len())
            .enumerate()
            .filter(|(start, window)| *start + marker.len() > prefix_len && *window == marker)
            .count();
        let keep_from = marker_prefix
            .len()
            .saturating_sub(marker.len().saturating_sub(1));
        marker_prefix.drain(..keep_from);

        tail.extend(&chunk[..read]);
        if tail.len() > limit {
            truncated = true;
            tail.drain(..tail.len() - limit);
        }
    }
    Ok(CapturedOutput {
        bytes: tail.into(),
        truncated,
        result_frames,
    })
}

fn captured_output(
    capture: std::thread::JoinHandle<std::io::Result<CapturedOutput>>,
    stream: &str,
) -> Result<CapturedOutput, String> {
    capture
        .join()
        .map_err(|_| format!("capturing {stream}: reader panicked"))?
        .map_err(|error| format!("capturing {stream}: {error}"))
}

fn framed_outcome(output: ProgramOutput) -> Outcome {
    let stdout = output.stdout;
    match (output.result_frames, stdout.split_once(BOX_RESULT_FRAME)) {
        (frames, _) if frames > 1 => {
            Outcome::Failed("the program printed more than one box result frame".to_string())
        }
        (1, Some((_, "OK"))) => Outcome::Pass,
        (1, Some((_, answer))) => Outcome::Failed(format!("box() answered {answer:?}")),
        (1, None) => Outcome::Failed(
            "the bounded stdout tail discarded the program's only box result frame".to_string(),
        ),
        (0, _) => Outcome::Failed(format!(
            "the program ended without framing an answer; stdout {stdout:?}"
        )),
        _ => Outcome::Failed("inconsistent box result-frame accounting".to_string()),
    }
}

#[cfg(test)]
fn complete_program_output(stdout: String) -> ProgramOutput {
    ProgramOutput {
        result_frames: stdout.matches(BOX_RESULT_FRAME).count(),
        stdout,
    }
}

/// Expand runner-provided declarations and recover the Kotlin source set represented by one corpus
/// file. The preamble stays the authoritative directive source; split blocks deliberately omit it.
pub fn prepare_sources(
    source: &str,
    fallback_stem: &str,
    target: TestTarget,
) -> Result<Vec<(String, String)>, Outcome> {
    let prepared = krusty::conformance::prepare_test_source(source, target);
    let mut sources = if source.contains("// FILE:") {
        let (kotlin, java) = krusty::conformance::split_files(&prepared);
        if !java.is_empty() {
            return Err(Outcome::NotApplicable(
                "contains Java source blocks unavailable to this target",
            ));
        }
        if kotlin.is_empty() {
            return Err(Outcome::NotApplicable("contains no Kotlin source blocks"));
        }
        kotlin
    } else {
        vec![(fallback_stem.to_string(), prepared)]
    };
    if krusty::conformance::directive(source, "WITH_COROUTINES") {
        sources.push((
            "CoroutineUtil".to_string(),
            krusty::conformance::COROUTINE_HELPERS.to_string(),
        ));
    }
    Ok(sources)
}

fn checked_outcome(file: &Path, result: &Outcome) -> Result<box_ratchet::Outcome, String> {
    if super::box_inventory::owned(file)
        && matches!(
            result,
            Outcome::Harness(_) | Outcome::CompilerPanic(_) | Outcome::NotApplicable(_)
        )
    {
        return Err(format!(
            "owned box case {} has fatal outcome: {result:?}",
            file.display()
        ));
    }
    Ok(ratchet_outcome(result))
}

#[test]
fn owned_fatal_outcomes_cannot_be_blessed_as_box_failures() {
    let file =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/box_regressions/multimodule/example.kt");
    for result in [
        Outcome::Harness("missing executable".into()),
        Outcome::CompilerPanic("panic".into()),
        Outcome::NotApplicable("target directive"),
    ] {
        assert_eq!(
            checked_outcome(&file, &result),
            Err(format!(
                "owned box case {} has fatal outcome: {result:?}",
                file.display()
            ))
        );
    }
    assert_eq!(
        checked_outcome(&file, &Outcome::Pass),
        Ok(box_ratchet::Outcome::Pass)
    );
    assert_eq!(
        checked_outcome(&file, &Outcome::Declined("unsupported".into())),
        Ok(box_ratchet::Outcome::Fail)
    );
    assert_eq!(
        checked_outcome(
            Path::new("/upstream/a.kt"),
            &Outcome::Harness("existing policy".into())
        ),
        Ok(box_ratchet::Outcome::Fail)
    );
}

fn outcome(lane: &dyn Lane, scratch: &Path, file: &Path) -> Outcome {
    let source = match std::fs::read_to_string(file) {
        Ok(source) => source,
        Err(error) => return Outcome::Harness(format!("reading case: {error}")),
    };
    let stem = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Case");
    if super::box_inventory::owned(file) {
        if let Err(error) = super::box_reference_classes::checked_box(&source, stem) {
            return Outcome::Harness(format!("required JVM oracle: {error}"));
        }
        if lane.platform() == box_ratchet::Platform::Native && source.contains("// MODULE:") {
            let answer =
                super::kotlin_native_multimodule_support::kotlin_native_multimodule_box(&source);
            if answer.as_deref() != Some("OK") {
                return Outcome::Harness(format!(
                    "required Native oracle must return OK, got {answer:?}"
                ));
            }
        }
    }
    if let Some(reason) = lane.not_applicable(&source) {
        return Outcome::NotApplicable(reason);
    }
    if let Some(reason) = lane.harness_limitation(&source) {
        return Outcome::Harness(reason.to_string());
    }
    if source.contains("// MODULE:") {
        let prepared = krusty::conformance::prepare_test_source(&source, lane.test_target());
        let Some(mut modules) = krusty::conformance::split_modules(&prepared) else {
            return Outcome::Harness("unsupported // MODULE: shape".to_string());
        };
        if krusty::conformance::directive(&source, "WITH_COROUTINES") {
            krusty::conformance::inject_support_module(
                &mut modules,
                krusty::conformance::COROUTINE_HELPERS,
            );
        }
        let units = krusty::conformance::module_units(&modules);
        if units.iter().any(|unit| !unit.java_files.is_empty()) {
            return Outcome::NotApplicable(
                "contains Java source blocks unavailable to this target",
            );
        }
        let mut emitted = BTreeSet::new();
        for unit in &units {
            if unit
                .deps
                .iter()
                .any(|dependency| !emitted.contains(dependency))
            {
                return Outcome::Harness("module dependency declared out of order".to_string());
            }
            emitted.insert(unit.name.clone());
        }
        return lane.compile_modules_and_run(scratch, stem, &units, &source);
    }
    let sources = match prepare_sources(&source, stem, lane.test_target()) {
        Ok(sources) => sources,
        Err(outcome) => return outcome,
    };
    lane.compile_and_run(scratch, stem, &sources, &source)
}

/// The report line, in a stable machine-readable shape:
/// `<lane> conformance: scanned S, passed P, declined D, frontend F, harness H, not-applicable N,
/// compiler-panics Q, known-failures K, failed X`.
fn report_line(lane: &str, counts: &[(&str, usize)]) -> String {
    let fields = counts
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{lane} conformance: {fields}")
}

/// A deliberate local bless may replace the committed outcome inventory only after every case completed
/// without a compiler panic. Panic details are reported below before the ordinary failure
/// assertion; keeping this decision separate prevents that reporting path from publishing a
/// partial inventory.
fn may_write_blessed_expectations(bless: bool, compiler_panics: usize) -> bool {
    bless && compiler_panics == 0
}

/// Run the whole corpus through `lane` and hold it to the lane's committed ratchet.
pub fn run(lane: &dyn Lane) {
    let env = |knob: &str| format!("KRUSTY_{}_{knob}", lane.env_infix());
    let label = lane.label();
    let unavailable = lane.unavailable();
    let provisioned = krusty::toolchain::box_corpus_dir().is_some();
    super::box_inventory::require_setup(unavailable.as_deref(), provisioned)
        .unwrap_or_else(|error| panic!("{label}: {error}"));
    let corpus = krusty::toolchain::box_corpus_dir().expect("validated corpus setup");
    let files = super::box_inventory::discover(&corpus).unwrap_or_else(|error| panic!("{error}"));
    let relative = |file: &Path| box_ratchet::corpus_key(&corpus, file);
    let platform = lane.platform();
    let version = krusty::kotlin_version::target();
    let corpus_cases: BTreeSet<String> = files.iter().map(|file| relative(file)).collect();
    // Triage knob: scan only the cases whose path contains this substring. The expected-failure
    // checks below cover the cases actually scanned, so a narrowed run stays self-consistent.
    let only = std::env::var(env("BOX_ONLY")).ok();
    let files = match &only {
        Some(pattern) => files
            .into_iter()
            .filter(|file| file.to_string_lossy().contains(pattern.as_str()))
            .collect(),
        None => files,
    };
    let limit = std::env::var(env("BOX_LIMIT"))
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);
    let files = krusty::conformance::evenly_sample(files, limit);
    let full_run = only.is_none() && limit == usize::MAX;
    let bless = box_ratchet::bless_requested(
        std::env::var("KRUSTY_BLESS_BOX_EXPECTATIONS")
            .ok()
            .as_deref(),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    validate_bless(
        bless,
        full_run,
        provisioned,
        std::env::var_os("CI").is_some(),
    )
    .unwrap_or_else(|error| panic!("{error}: {label}"));
    let baseline = provisioned.then(|| box_ratchet::load(platform, version));
    let scratch = Scratch::new(platform.name());
    // Panics are recorded per case below; the default hook's backtrace spam would bury the
    // report. The hook keeps the panic's file so the case can be attributed.
    std::panic::set_hook(Box::new(|info| {
        let file = info.location().map(|location| location.file().to_string());
        LAST_PANIC_FILE.with(|last| *last.borrow_mut() = file);
    }));
    // The same worker pool shape as the JVM gate: frontend analysis dominates each case, and the
    // deep recursion of resolution wants a wide stack.
    let mut pool = rayon::ThreadPoolBuilder::new().stack_size(64 * 1024 * 1024);
    if let Some(threads) = std::env::var("KRUSTY_TEST_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        pool = pool.num_threads(threads);
    }
    let pool = pool.build().expect("build the worker pool");

    let started = Instant::now();
    let outcomes: Vec<(&PathBuf, Outcome)> = pool.install(|| {
        files
            .par_iter()
            .map(|file| {
                // A panic inside the compiler is a failure of THAT case, recorded like any other
                // so one bug does not hide the picture for the rest of the corpus; the test still
                // fails.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    outcome(lane, &scratch.0, file)
                }))
                .unwrap_or_else(|panic| {
                    let message = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_else(|| "panic".to_string());
                    let origin = LAST_PANIC_FILE.with(|last| last.borrow_mut().take());
                    Outcome::CompilerPanic(match origin {
                        Some(origin) => format!("{message} ({origin})"),
                        None => message,
                    })
                });
                (file, result)
            })
            .collect()
    });
    // Compilation is finished. Restore ordinary panic reporting before validating outcomes:
    // an owned fatal outcome or a broken inventory must remain visible, not fail silently under
    // the compiler-only hook.
    let _ = std::panic::take_hook();

    let mut passed = 0usize;
    let mut frontend = 0usize;
    let mut declined: BTreeMap<String, usize> = BTreeMap::new();
    let mut frontend_reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut harness_reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut not_applicable: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut traced: Vec<(PathBuf, String)> = Vec::new();
    let mut compiler_panics: Vec<(PathBuf, String)> = Vec::new();
    let mut known_failures: Vec<(PathBuf, String)> = Vec::new();
    let mut failed: Vec<(PathBuf, String)> = Vec::new();
    let mut current = BTreeMap::new();
    for (file, result) in outcomes {
        let relative = relative(file);
        let observed = checked_outcome(file, &result).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            current.insert(relative.clone(), observed).is_none(),
            "corpus contains the duplicate path {relative:?}"
        );
        match result {
            Outcome::Pass => passed += 1,
            Outcome::Frontend(reason) => {
                frontend += 1;
                // Traced beside the declines: a ranked reason with no case behind it cannot be
                // reproduced. Kept VERBATIM, unlike a decline: almost every one of these is an
                // unresolved reference, and there the name is the whole of the information.
                traced.push((file.clone(), reason.clone()));
                *frontend_reasons.entry(reason).or_default() += 1;
            }
            Outcome::CompilerPanic(reason) => {
                compiler_panics.push((file.clone(), reason.clone()));
                failed.push((file.clone(), format!("compiler panic: {reason}")));
            }
            Outcome::Declined(reason) => {
                traced.push((file.clone(), reason.clone()));
                *declined.entry(reason).or_default() += 1;
            }
            Outcome::Harness(reason) => *harness_reasons.entry(reason).or_default() += 1,
            Outcome::NotApplicable(reason) => *not_applicable.entry(reason).or_default() += 1,
            Outcome::Failed(reason) => {
                let expected = baseline
                    .as_ref()
                    .is_some_and(|baseline| baseline.failures.contains(&relative));
                if expected || bless {
                    known_failures.push((file.clone(), reason));
                } else if !provisioned {
                    failed.push((file.clone(), reason));
                }
            }
        }
    }
    if may_write_blessed_expectations(bless, compiler_panics.len()) {
        box_ratchet::write(platform, version, &current)
            .unwrap_or_else(|error| panic!("failed to write {label} box expectations: {error}"));
    }
    if !bless {
        if let Some(baseline) = &baseline {
            let mismatches = box_ratchet::compare(baseline, &current, &corpus_cases);
            if !mismatches.is_empty() {
                failed.push((
                    PathBuf::from(format!("<{} box expectations>", platform.name())),
                    mismatches.report(platform, version),
                ));
            }
        }
    }
    failed.sort();
    known_failures.sort();
    compiler_panics.sort();

    let declined_total: usize = declined.values().sum();
    let harness_total: usize = harness_reasons.values().sum();
    let not_applicable_total: usize = not_applicable.values().sum();
    let applicable = files.len() - not_applicable_total;
    let report = report_line(
        platform.name(),
        &[
            ("scanned", files.len()),
            ("passed", passed),
            ("declined", declined_total),
            ("frontend", frontend),
            ("harness", harness_total),
            ("not-applicable", not_applicable_total),
            ("compiler-panics", compiler_panics.len()),
            ("known-failures", known_failures.len()),
            ("failed", failed.len()),
        ],
    );
    eprintln!("{report} ({:.1}s)", started.elapsed().as_secs_f64());

    // Triage: name the cases behind one decline reason, so a line in the table below can be
    // followed back to the source that produced it.
    if let Ok(pattern) = std::env::var(env("BOX_TRACE")) {
        for (file, reason) in &traced {
            if reason.contains(&pattern) {
                eprintln!("  traced {}: {reason}", file.display());
            }
        }
    }

    // What the FRONTEND refuses. Every case here is core work — nothing about it is this
    // backend's — and every backend would gain from it, which is why it is ranked beside the
    // declines rather than left as a single number.
    let mut frontend_ranked: Vec<(&String, &usize)> = frontend_reasons.iter().collect();
    frontend_ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (reason, count) in frontend_ranked.iter().take(60) {
        eprintln!("  frontend {count:>5}  {reason}");
    }

    // The backlog: what the generator declines, most frequent first.
    let mut reasons: Vec<(&String, &usize)> = declined.iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (reason, count) in reasons.iter().take(40) {
        eprintln!("  declined {count:>5}  {reason}");
    }
    for (reason, count) in &harness_reasons {
        eprintln!("  harness {count:>5}  {reason}");
    }
    for (reason, count) in &not_applicable {
        eprintln!("  not applicable {count:>5}  {reason}");
    }
    if let Ok(path) = std::env::var(env("CONFORMANCE_REPORT")) {
        std::fs::write(
            path,
            super::common::conformance_report::count_report(passed as u64, applicable as u64),
        )
        .unwrap_or_else(|error| panic!("failed to write {label} conformance report: {error}"));
    }

    for (file, reason) in &compiler_panics {
        eprintln!("  compiler panic {}: {reason}", file.display());
    }
    for (file, reason) in &known_failures {
        eprintln!("  known failure {}: {reason}", file.display());
    }
    for (file, reason) in &failed {
        eprintln!("FAILED {}: {reason}", file.display());
    }
    assert!(
        failed.is_empty(),
        "{} {label} conformance failure(s): every applicable non-pass must match the committed \
         expected-failure inventory, and compiler panics are never expected",
        failed.len()
    );
}

#[cfg(unix)]
fn executable_script(scratch: &Scratch, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = scratch.0.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write runner fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make runner fixture executable");
    path
}

#[test]
fn source_sets_split_files_and_receive_runner_helpers() {
    let sources = prepare_sources(
        "// WITH_COROUTINES\n// FILE: lib.kt\nfun answer() = \"OK\"\n\
         // FILE: main.kt\nfun box() = answer()\n",
        "fallback",
        TestTarget::Native,
    )
    .expect("Kotlin-only multi-file source set");
    assert_eq!(
        sources
            .iter()
            .map(|(stem, _)| stem.as_str())
            .collect::<Vec<_>>(),
        ["lib", "main", "CoroutineUtil"]
    );
    assert!(sources[2].1.contains("fun <T> runBlocking"));
}

#[test]
fn a_depends_on_source_set_enables_the_multiplatform_project_feature() {
    assert!(!module_features("", 0).has("MultiPlatformProjects"));
    assert!(module_features("", 1).has("MultiPlatformProjects"));
}

#[test]
fn a_warning_does_not_reject_a_box_case_but_an_error_does() {
    let mut diags = DiagSink::new();
    let span = krusty::diag::Span { lo: 3, hi: 4 };
    diags.warning(span, "warning only");
    assert_eq!(first_error(&diags, 0).map(|diag| diag.msg.as_str()), None);

    let error_start = diags.diags.len();
    diags.error(span, "frontend rejection");
    assert_eq!(
        first_error(&diags, 0).map(|diag| (diag.severity, diag.msg.as_str())),
        Some((Severity::Error, "frontend rejection"))
    );
    assert_eq!(
        first_error(&diags, error_start).map(|diag| diag.msg.as_str()),
        Some("frontend rejection")
    );
}

#[test]
fn blessing_is_a_deliberate_local_full_corpus_operation() {
    assert_eq!(validate_bless(false, false, false, true), Ok(()));
    assert_eq!(validate_bless(true, true, true, false), Ok(()));
    assert!(validate_bless(true, false, true, false).is_err());
    assert!(validate_bless(true, true, false, false).is_err());
    assert!(validate_bless(true, true, true, true).is_err());
}

#[test]
fn a_bless_with_a_compiler_panic_cannot_write_expectations() {
    assert_eq!(
        [(false, 0), (false, 1), (true, 1), (true, 0),]
            .map(|(bless, compiler_panics)| may_write_blessed_expectations(bless, compiler_panics)),
        [false, false, false, true]
    );
}

#[test]
fn source_sets_do_not_flatten_java_into_kotlin() {
    assert_eq!(
        prepare_sources(
            "// FILE: J.java\nclass J {}\n// FILE: main.kt\nfun box() = \"OK\"\n",
            "fallback",
            TestTarget::Native,
        ),
        Err(Outcome::NotApplicable(
            "contains Java source blocks unavailable to this target"
        ))
    );
}

#[cfg(unix)]
#[test]
fn runner_does_not_block_on_large_stdout_or_stderr() {
    let scratch = Scratch::new("runner");
    let executable = executable_script(
        &scratch,
        "large-output",
        "i=0\nwhile [ \"$i\" -lt 20000 ]; do\n  printf '0123456789abcdef0123456789abcdef\\n'\n  printf 'fedcba9876543210fedcba9876543210\\n' >&2\n  i=$((i + 1))\ndone\nprintf 'done'",
    );

    let output = run_with_timeout(&mut Command::new(&executable), Duration::from_secs(5))
        .expect("large output must not fill a pipe and deadlock");

    assert!(
        output.stdout.len() > 600_000,
        "the complete stdout is retained"
    );
    assert!(
        output.stdout.ends_with("done"),
        "the final answer is retained"
    );
}

#[test]
fn a_missing_program_is_a_harness_error_not_a_recordable_failure() {
    let scratch = Scratch::new("missing-program");
    let missing = scratch.0.join("not-an-executable");
    let error = Command::new(&missing).output().unwrap_err();
    assert_eq!(
        run_program(&mut Command::new(&missing)),
        Outcome::Harness(format!("could not start the program: {error}"))
    );
}

#[test]
fn runner_bounds_output_while_retaining_the_verdict_tail() {
    let mut output = vec![b'x'; 80];
    output.extend_from_slice(BOX_RESULT_FRAME.as_bytes());
    output.extend_from_slice(b"OK");

    let split = 80 + BOX_RESULT_FRAME.len() / 2;
    let input =
        std::io::Cursor::new(&output[..split]).chain(std::io::Cursor::new(&output[split..]));
    let captured = output_tail_with_limit(input, 64).expect("split-frame output capture succeeds");

    assert!(captured.truncated);
    assert_eq!(captured.bytes.len(), 64);
    assert!(captured
        .bytes
        .ends_with(format!("{BOX_RESULT_FRAME}OK").as_bytes()));
    assert_eq!(captured.result_frames, 1);
}

#[test]
fn a_frame_discarded_from_the_bounded_tail_still_invalidates_the_verdict() {
    let frame = BOX_RESULT_FRAME;
    let mut output = format!("{frame}forged").into_bytes();
    output.extend(std::iter::repeat_n(b'x', 80));
    output.extend_from_slice(format!("{frame}OK").as_bytes());
    let captured = output_tail_with_limit(std::io::Cursor::new(output), 64)
        .expect("in-memory output capture succeeds");
    assert!(captured.truncated);
    assert_eq!(captured.result_frames, 2);
    assert_eq!(
        framed_outcome(ProgramOutput {
            stdout: String::from_utf8(captured.bytes).expect("ASCII fixture"),
            result_frames: captured.result_frames,
        }),
        Outcome::Failed("the program printed more than one box result frame".to_string())
    );
}

#[cfg(unix)]
#[test]
fn runner_kills_a_program_at_its_deadline() {
    let scratch = Scratch::new("runner");
    let executable = executable_script(&scratch, "timeout", "while :; do :; done");

    assert_eq!(
        run_with_timeout(&mut Command::new(&executable), Duration::from_millis(20)),
        Err(ProgramRunError::Program(
            "the program did not finish within 0s".to_string()
        ))
    );
}

#[test]
fn a_multiline_box_answer_cannot_hide_behind_a_final_ok_line() {
    let stdout = format!("{BOX_RESULT_FRAME}FAIL\nOK");
    assert_eq!(
        framed_outcome(complete_program_output(stdout)),
        Outcome::Failed("box() answered \"FAIL\\nOK\"".to_string())
    );
}

#[test]
fn a_box_answer_cannot_forge_success_with_an_embedded_frame() {
    let stdout = format!("{BOX_RESULT_FRAME}FAIL{BOX_RESULT_FRAME}OK");
    assert_eq!(
        framed_outcome(complete_program_output(stdout)),
        Outcome::Failed("the program printed more than one box result frame".to_string())
    );
}

#[test]
fn report_line_has_a_stable_shape() {
    assert_eq!(
        report_line("native", &[("scanned", 3), ("passed", 1)]),
        "native conformance: scanned 3, passed 1"
    );
}
