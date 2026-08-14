//! Shared test helpers.

use std::collections::{HashMap, VecDeque};
use std::io::{Read as _, Write as _};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use krusty::jvm::classpath::Classpath;

/// Locate the batch CLI built from the separate `krusty-cli` workspace package.
///
/// The canonical test runner builds it before starting the suite. A direct `cargo test -p krusty`
/// may not, so build it once on demand in the test profile rather than coupling the compiler crate
/// back to the executable package.
#[allow(dead_code)]
pub fn krusty_binary() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            if let Some(path) = std::env::var_os("KRUSTY_BIN") {
                return PathBuf::from(path);
            }

            let current = std::env::current_exe().expect("locate current test executable");
            let profile_dir = current
                .parent()
                .and_then(Path::parent)
                .expect("test executable must be under target/<profile>/deps");
            let binary = profile_dir.join(format!("krusty{}", std::env::consts::EXE_SUFFIX));
            let profile = profile_dir
                .file_name()
                .and_then(|name| name.to_str())
                .expect("target profile directory must be UTF-8");
            let mut build = Command::new(env!("CARGO"));
            build.args(["build", "-p", "krusty-cli"]);
            if profile != "debug" {
                build.args(["--profile", profile]);
            }
            let status = build
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .status()
                .expect("build krusty-cli");
            assert!(status.success(), "failed to build krusty-cli");
            assert!(
                binary.is_file(),
                "krusty-cli did not produce {}",
                binary.display()
            );
            binary
        })
        .clone()
}

/// Make a spawned child receive `SIGKILL` when THIS test process dies, so a persistent JVM runner is
/// killed at teardown (clean exit OR the gate SIGKILL-ing the binary) instead of orphaning ~1 GB.
/// Linux `PR_SET_PDEATHSIG`; a no-op elsewhere. MUST be paired with [`spawn_owned`]: on Linux the
/// parent-death signal fires when the CREATING THREAD exits, not the process — and libtest worker
/// threads come and go per test, so a JVM spawned directly from one is SIGKILL'd the moment that test's
/// thread ends, forcing a cold respawn (measured: a reference compile ballooning 1.5s → 40-120s). Spawning
/// from the single immortal owner thread instead binds the signal to a thread that lives for the whole
/// process, so it fires only at real teardown.
#[allow(dead_code)]
fn die_with_parent(_cmd: &mut Command) {
    // `_cmd` is consumed only by the Linux `pre_exec` below; on every other target the body compiles
    // out, so the leading underscore keeps it from reading as an unused parameter there.
    #[cfg(target_os = "linux")]
    // SAFETY: `pre_exec` runs in the forked child before `exec`; `prctl` is async-signal-safe and touches
    // no shared state, so it is valid in that restricted context.
    unsafe {
        use std::os::unix::process::CommandExt;
        _cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
            Ok(())
        });
    }
}

/// Spawn a child process from ONE immortal owner thread shared by the whole binary, so a `PR_SET_PDEATHSIG`
/// set on it (see [`die_with_parent`]) binds to a thread that never exits until process teardown — not to
/// a transient libtest worker thread. Every persistent JVM (box runner, kotlinc/java servers) MUST spawn
/// through here; without it the parent-death signal mis-fires per test and the JVMs churn.
#[allow(dead_code)]
fn spawn_owned(cmd: Command) -> std::io::Result<Child> {
    type Job = Box<dyn FnOnce() + Send>;
    static OWNER: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    let tx = OWNER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        // The immortal owner thread: it only ever runs spawn closures, so every child it forks is parented
        // to it. It never returns (the receiver stays open via the retained `tx`), so PDEATHSIG fires only
        // when the process itself dies.
        std::thread::Builder::new()
            .name("jvm-spawner".into())
            .spawn(move || {
                for job in rx {
                    job();
                }
            })
            .expect("spawn jvm-spawner thread");
        Mutex::new(tx)
    });
    let (rtx, rrx) = mpsc::channel();
    let job: Job = Box::new(move || {
        let mut cmd = cmd;
        let _ = rtx.send(cmd.spawn());
    });
    tx.lock().unwrap_or_else(|e| e.into_inner()).send(job).ok();
    rrx.recv()
        .unwrap_or_else(|_| Err(std::io::Error::other("jvm-spawner gone")))
}

/// Per-phase timing guard for e2e profiling, gated by the `KRUSTY_PROF` env var (off by default so a
/// normal run pays nothing). On drop it prints `PROF\t<phase>\t<ms>` to stderr; aggregate the lines to see
/// where the e2e wall clock goes (krusty compile vs real-kotlinc dep build vs JVM `box()` round-trip).
#[allow(dead_code)]
struct ProfGuard {
    phase: &'static str,
    start: Instant,
    on: bool,
}
#[allow(dead_code)]
impl ProfGuard {
    fn new(phase: &'static str) -> Self {
        Self {
            phase,
            start: Instant::now(),
            on: std::env::var_os("KRUSTY_PROF").is_some(),
        }
    }
}
impl Drop for ProfGuard {
    fn drop(&mut self) {
        if self.on {
            eprintln!("PROF\t{}\t{}", self.phase, self.start.elapsed().as_millis());
        }
    }
}

fn scratch_namespace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/scratch")
}

fn stale_scratch_roots() -> [PathBuf; 2] {
    [
        scratch_namespace(),
        std::env::temp_dir().join("krusty_scratch"),
    ]
}

/// Allocate a unique directory below this process's private scratch root.
#[allow(dead_code)]
pub fn scratch_dir() -> Option<PathBuf> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let root = ROOT
        .get_or_init(|| {
            sweep_stale_temp_dirs();
            let base = scratch_namespace();
            std::fs::create_dir_all(&base).ok()?;
            let pid = std::process::id();
            let mut suffix = 0u64;
            loop {
                let root = base.join(if suffix == 0 {
                    pid.to_string()
                } else {
                    format!("{pid}_{suffix}")
                });
                match std::fs::create_dir(&root) {
                    Ok(()) => break Some(root),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        suffix += 1;
                    }
                    Err(_) => break None,
                }
            }
        })
        .as_ref()?;
    let dir = root.join(NEXT.fetch_add(1, Ordering::Relaxed).to_string());
    std::fs::create_dir(&dir).ok()?;
    Some(dir)
}

/// Remove scratch directories whose owner PID is no longer live.
#[allow(dead_code)]
fn sweep_stale_temp_dirs() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        for root in stale_scratch_roots() {
            let Ok(rd) = std::fs::read_dir(root) else {
                continue;
            };
            for e in rd.flatten() {
                let Some(pid) = scratch_owner_pid(&e.file_name()) else {
                    continue;
                };
                if temp_dir_owner_is_dead(pid) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    });
}

fn scratch_owner_pid(name: &std::ffi::OsStr) -> Option<i32> {
    let name = name.to_str()?;
    let pid = match name.split_once('_') {
        Some((pid, suffix))
            if !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            pid
        }
        Some(_) => return None,
        None => name,
    };
    pid.parse().ok()
}

/// Only `ESRCH` proves that an owner is dead; `EPERM` can describe a live process.
#[allow(dead_code)]
fn temp_dir_owner_is_dead(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return false;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn parse_source_set(
    sources: &[&str],
    diags: &mut krusty::diag::DiagSink,
) -> Option<Vec<krusty::ast::File>> {
    let mut files = sources
        .iter()
        .map(|source| {
            let features = krusty::features::LangFeatures::from_source(source);
            let tokens = krusty::lexer::lex(source, diags);
            krusty::parser::parse_with_features(source, &tokens, diags, &features)
        })
        .collect::<Vec<_>>();
    if diags.has_errors() {
        return None;
    }
    if sources.iter().any(|source| {
        krusty::features::LangFeatures::from_source(source).has("MultiPlatformProjects")
    }) {
        krusty::frontend::strip_matched_expects(&mut files);
    }
    Some(files)
}

fn cached_classpath(cp_jars: &[PathBuf], jdk_modules: Option<&Path>) -> std::rc::Rc<Classpath> {
    let mut paths = cp_jars.to_vec();
    if let Some(path) = jdk_modules {
        paths.push(path.to_path_buf());
    }
    thread_local! {
        static CACHE: std::cell::RefCell<
            std::collections::HashMap<Vec<PathBuf>, std::rc::Rc<Classpath>>,
        > = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry(paths.clone())
            .or_insert_with(|| std::rc::Rc::new(Classpath::new(paths)))
            .clone()
    })
}

/// Compile Kotlin `src` to `(internal_name, class_bytes)` pairs entirely in-process — the same pipeline
/// (`lex → parse → check → ir_lower → ir_emit`) the conformance harness uses, sharing the process-global
/// classpath caches (type/ext/jimage indexes) across every call. This is dramatically faster than
/// spawning the `krusty` binary once per snippet (each subprocess rebuilds those indexes from scratch).
/// `cp_jars` are the `-classpath` jars; `jdk_modules` is the JDK `lib/modules` jimage (the bootclasspath).
/// Returns `None` on any compile error (an unsupported feature), like the CLI's non-zero exit.
pub fn compile_in_process(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<Vec<(String, Vec<u8>)>> {
    use krusty::diag::DiagSink;
    use krusty::frontend::{check_file, collect_signatures_with_cp};
    use krusty::jvm::names::file_class_name;

    let _pg = ProfGuard::new("krusty");
    let mut diags = DiagSink::new();
    let files = parse_source_set(&[src], &mut diags)?;
    let cp = cached_classpath(cp_jars, jdk_modules);
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let file = &files[0];
    let info = check_file(file, &mut syms, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let facade = file_class_name(stem, file.package.as_deref());
    let runtime = krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone());
    let mut ir = krusty::ir_lower::lower_file(file, &info, &syms, &runtime)?;
    // The real backend's shared post-lowering pass pipeline (plugins → value-classes → suspend →
    // must-inline marks → lambda reparenting) — one definition, so `compile_in_process` can't drift
    // from what ships. An unlowerable shape → skip, don't miscompile.
    krusty::jvm::backend::run_backend_passes(&mut ir, file, &facade, "main", &syms).ok()?;
    // The facade `@Metadata` the CLI backend writes (top-level fn/extension records) — without it a
    // SEPARATE compilation reading this output from the classpath cannot resolve extensions.
    let metadata = krusty::jvm::backend::facade_package_metadata(file, 0, &syms);
    // The SHIPPING emit config (per-class `@Metadata`, `SourceFile`, …) — the same definition the CLI
    // backend uses, so this helper can't drift from what `krusty -d …` writes.
    let opts = krusty::jvm::backend::shipping_emit_options(stem, "main", None, cp.clone());
    let outputs = krusty::jvm::ir_emit::emit_all_with_opts(
        &ir,
        &facade,
        &*cp,
        metadata.as_ref(),
        &opts,
        &krusty::jvm::ir_emit::EmitRun::default(),
        &syms,
    )?;
    if outputs.is_empty() {
        None
    } else {
        Some(outputs)
    }
}

/// Compile a source set through the module-wide compiler driver.
#[allow(dead_code)]
pub fn compile_in_process_files(
    sources: &[(&str, &str)],
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<Vec<(String, Vec<u8>)>> {
    use krusty::diag::DiagSink;
    use krusty::frontend::collect_signatures_with_cp;

    let _pg = ProfGuard::new("krusty");
    let mut diags = DiagSink::new();
    let source_texts = sources
        .iter()
        .map(|(_, source)| *source)
        .collect::<Vec<_>>();
    let files = parse_source_set(&source_texts, &mut diags)?;
    let cp = cached_classpath(cp_jars, jdk_modules);
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let mut symbols = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let stems = sources
        .iter()
        .map(|(stem, _)| (*stem).to_string())
        .collect::<Vec<_>>();
    krusty::jvm::prepare_module_symbols(&files, &stems, &mut symbols);
    let backend = krusty::jvm::JvmBackend::new(cp);
    let outputs =
        krusty::compiler::compile(&files, &stems, &mut symbols, &backend, "main", &mut diags);
    let classes = outputs
        .into_iter()
        .map(|(path, bytes)| {
            (
                path.strip_suffix(".class").unwrap_or(&path).to_string(),
                bytes,
            )
        })
        .collect::<Vec<_>>();
    (!diags.has_errors() && !classes.is_empty()).then_some(classes)
}

/// Like [`compile_in_process`], but retaining the suspend pass's continuation metadata for emission,
/// and pinning per-class `@kotlin.Metadata` ON explicitly rather than inheriting the shipping default
/// — the byte-identity tests must exercise the computed payload whatever
/// [`krusty::jvm::ir_emit::EmitOptions::emit_class_metadata`] defaults to (and whatever
/// `KRUSTY_NO_CLASS_METADATA` is set to in the environment).
///
/// Takes a classpath (for sources referencing the kotlin stdlib or other jars) and stamps the
/// `SourceFile` (`<stem>.kt`) exactly as the CLI backend does, so the emitted bytes match a
/// `krusty -d …` (and thus kotlinc) run WITHOUT spawning a subprocess. This is the in-process path the
/// byte-identity tests use, so their whole codepath is coverage-instrumented (a spawned CLI is not)
/// and pays no per-run cold classpath scan.
#[allow(dead_code)]
pub fn compile_in_process_metadata_cp(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
) -> Option<Vec<(String, Vec<u8>)>> {
    use krusty::diag::DiagSink;
    use krusty::frontend::{check_file, collect_signatures_with_cp};
    use krusty::jvm::ir_emit::EmitRun;
    use krusty::jvm::names::file_class_name;

    let _pg = ProfGuard::new("krusty");
    let mut diags = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(src);
    let toks = krusty::lexer::lex(src, &mut diags);
    let files = vec![krusty::parser::parse_with_features(
        src, &toks, &mut diags, &features,
    )];
    if diags.has_errors() {
        return None;
    }
    let cp = std::rc::Rc::new(Classpath::new(cp_jars.to_vec()));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let file = &files[0];
    let info = check_file(file, &mut syms, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let facade = file_class_name(stem, file.package.as_deref());
    let runtime = krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone());
    let mut ir = krusty::ir_lower::lower_file(file, &info, &syms, &runtime)?;
    let mut continuation_metadata = krusty::jvm::suspend::ContinuationMetadataMap::default();
    krusty::jvm::backend::run_backend_passes_with_metadata(
        &mut ir,
        file,
        &facade,
        "main",
        &syms,
        &mut continuation_metadata,
    )
    .ok()?;
    let mut opts = krusty::jvm::backend::shipping_emit_options(stem, "main", None, cp.clone());
    // This differential helper tests the metadata writer itself, so keep that feature enabled even
    // when `KRUSTY_NO_CLASS_METADATA` asks shipping callers to omit it for a diagnostic bisect. All
    // other fields still come from the shared shipping constructor and therefore cannot drift.
    opts.emit_class_metadata = true;
    let run = EmitRun::default();
    // Facade `@Metadata` (k = 2, top-level fn/extension records), exactly as the CLI backend and
    // `compile_in_process` pass it — `None` for a class-only source, so today's byte-identity
    // fixtures are unaffected, and a future fixture mixing a class with top-level functions gets
    // the same facade record a real build would.
    let metadata = krusty::jvm::backend::facade_package_metadata(file, 0, &syms);
    let outputs = krusty::jvm::ir_emit::emit_all_with_opts_and_metadata(
        &ir,
        &facade,
        &*cp,
        krusty::jvm::ir_emit::EmitMetadata {
            facade: metadata.as_ref(),
            continuations: &continuation_metadata,
        },
        &opts,
        &run,
        &syms,
    )?;
    (!outputs.is_empty()).then_some(outputs)
}

/// Compile Kotlin `src` in-process and write the emitted `.class` files under `out_dir`, preserving
/// package directories. This matches the classfile layout the CLI writes for tests that need to run a
/// Java driver against krusty output, without paying a subprocess/compiler-cache cold start per case.
#[allow(dead_code)]
pub fn compile_to_dir(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
    out_dir: &Path,
) -> Option<()> {
    let classes = compile_in_process(src, stem, cp_jars, jdk_modules)?;
    // The facade's internal name gives the package → facade mapping for the module index below.
    // (`emit_all` outputs only classes; the `META-INF/<module>.kotlin_module` is a whole-module
    // artifact `compiler::compile` writes, so this dir-shaped helper reconstructs it — the real
    // kotlinc DISCOVERS a package's top-level declarations exclusively through that index, so a
    // classpath dir without one makes every facade `@Metadata` record invisible to it.)
    let facade =
        krusty::jvm::names::file_class_name(stem, parse_package_of_first_file(src).as_deref());
    let facade_emitted = classes.iter().any(|(name, _)| *name == facade);
    for (name, bytes) in classes {
        let path = out_dir.join(format!("{name}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(path, bytes).ok()?;
    }
    // kotlinc writes the module file even for a class-only compilation — with an EMPTY parts list
    // (a facade that was never emitted must not be listed).
    let packages: Vec<(String, Vec<String>)> = if facade_emitted {
        let (pkg, short) = match facade.rsplit_once('/') {
            Some((p, s)) => (p.replace('/', "."), s.to_string()),
            None => (String::new(), facade),
        };
        vec![(pkg, vec![short])]
    } else {
        Vec::new()
    };
    let module_bytes = krusty::metadata::module::build_kotlin_module(&packages);
    let meta_inf = out_dir.join("META-INF");
    std::fs::create_dir_all(&meta_inf).ok()?;
    std::fs::write(meta_inf.join("main.kotlin_module"), module_bytes).ok()?;
    Some(())
}

/// The `package` declaration of a single-file source, via the real parser (no textual scraping).
fn parse_package_of_first_file(src: &str) -> Option<String> {
    let mut diags = krusty::diag::DiagSink::new();
    let files = parse_source_set(&[src], &mut diags)?;
    files.first().and_then(|f| f.package.clone())
}

/// Run the same checked-file → JVM-backend pipeline as the CLI, but report whether the already
/// front-end-valid source is declined by a backend unsupported-feature path. Returns `None` when the
/// front end rejects the source, because backend-rejection tests should not pass via a parser/type
/// error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendOutcome {
    Emitted,
    LowerBail(String),
    BackendPassBail(krusty::jvm::backend::SkipReason),
    EmitBail,
}

#[allow(dead_code)]
pub fn backend_rejects_in_process(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<bool> {
    backend_outcome_in_process(src, stem, cp_jars, jdk_modules)
        .map(|outcome| outcome != BackendOutcome::Emitted)
}

#[allow(dead_code)]
pub fn backend_outcome_in_process(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<BackendOutcome> {
    use krusty::diag::DiagSink;
    use krusty::frontend::{check_file, collect_signatures_with_cp};
    use krusty::jvm::names::file_class_name;

    let mut diags = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(src);
    let toks = krusty::lexer::lex(src, &mut diags);
    let mut files = vec![krusty::parser::parse_with_features(
        src, &toks, &mut diags, &features,
    )];
    if diags.has_errors() {
        return None;
    }
    // Multiplatform: a matched `expect` header is replaced by its `actual` in the same set.
    if features.has("MultiPlatformProjects") {
        krusty::frontend::strip_matched_expects(&mut files);
    }
    let mut cp_paths: Vec<PathBuf> = cp_jars.to_vec();
    if let Some(p) = jdk_modules {
        cp_paths.push(p.to_path_buf());
    }
    thread_local! {
        static CP: std::cell::RefCell<std::collections::HashMap<Vec<PathBuf>, std::rc::Rc<Classpath>>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let cp = CP.with(|c| {
        c.borrow_mut()
            .entry(cp_paths.clone())
            .or_insert_with(|| std::rc::Rc::new(Classpath::new(cp_paths.clone())))
            .clone()
    });
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let file = &files[0];
    let info = check_file(file, &mut syms, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let facade = file_class_name(stem, file.package.as_deref());
    let runtime = krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone());
    let bail = std::cell::RefCell::new(String::new());
    let Some(mut ir) = krusty::ir_lower::lower_file_reporting(file, &info, &syms, &runtime, &bail)
    else {
        return Some(BackendOutcome::LowerBail(bail.borrow().clone()));
    };
    if let Err(reason) =
        krusty::jvm::backend::run_backend_passes(&mut ir, file, &facade, "main", &syms)
    {
        return Some(BackendOutcome::BackendPassBail(reason));
    }
    Some(
        if krusty::jvm::ir_emit::emit_all(&ir, &facade, &*cp, None, &syms).is_none() {
            BackendOutcome::EmitBail
        } else {
            BackendOutcome::Emitted
        },
    )
}

/// Lower Kotlin `src` to backend-agnostic IR (`lex → parse → check → collect → ir_lower`), stopping
/// before any JVM-specific pass — the exact input the alternate (`js`) backend consumes. Returns
/// `None` on a front-end error (caller skips). Shares the same thread-local `Classpath` cache as
/// `compile_in_process`.
#[allow(dead_code)]
pub fn lower_to_ir(
    src: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<krusty::ir::IrFile> {
    use krusty::diag::DiagSink;
    use krusty::frontend::{check_file, collect_signatures_with_cp};

    let mut diags = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(src);
    let toks = krusty::lexer::lex(src, &mut diags);
    let mut files = vec![krusty::parser::parse_with_features(
        src, &toks, &mut diags, &features,
    )];
    if diags.has_errors() {
        return None;
    }
    // Multiplatform: a matched `expect` header is replaced by its `actual` in the same set.
    if features.has("MultiPlatformProjects") {
        krusty::frontend::strip_matched_expects(&mut files);
    }
    let mut cp_paths: Vec<PathBuf> = cp_jars.to_vec();
    if let Some(p) = jdk_modules {
        cp_paths.push(p.to_path_buf());
    }
    let cp = std::rc::Rc::new(Classpath::new(cp_paths));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let file = &files[0];
    let info = check_file(file, &mut syms, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let runtime = krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone());
    krusty::ir_lower::lower_file(file, &info, &syms, &runtime)
}

#[allow(dead_code)]
pub fn compile_js_in_process(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<String> {
    use krusty::diag::DiagSink;
    use krusty::frontend::collect_signatures_with_cp;

    let mut diags = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(src);
    let toks = krusty::lexer::lex(src, &mut diags);
    let mut files = vec![krusty::parser::parse_with_features(
        src, &toks, &mut diags, &features,
    )];
    if diags.has_errors() {
        return None;
    }
    // Multiplatform: a matched `expect` header is replaced by its `actual` in the same set.
    if features.has("MultiPlatformProjects") {
        krusty::frontend::strip_matched_expects(&mut files);
    }
    let mut cp_paths: Vec<PathBuf> = cp_jars.to_vec();
    if let Some(p) = jdk_modules {
        cp_paths.push(p.to_path_buf());
    }
    let cp = std::rc::Rc::new(Classpath::new(cp_paths));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let stems = vec![stem.to_string()];
    let runtime = krusty::jvm::jvm_libraries::JvmLibraries::new(cp);
    let backend = krusty::js::JsBackend::new(runtime);
    let outputs =
        krusty::compiler::compile(&files, &stems, &mut syms, &backend, "main", &mut diags);
    if diags.has_errors() {
        return None;
    }
    outputs
        .into_iter()
        .find(|(path, _)| path == &format!("{stem}.js"))
        .and_then(|(_, bytes)| String::from_utf8(bytes).ok())
}

/// Run the front end (`lex → parse → collect signatures → check`) on `src` and return every
/// diagnostic message it produced (parse errors, then resolve/check errors). Empty ⇒ the snippet is
/// accepted. Lets tests exercise ERROR paths — assert a bad snippet yields a diagnostic (optionally
/// matching a substring). `cp_jars`/`jdk_modules` supply the resolution classpath, like the box
/// helpers; pass `&[]`/`None` for snippets that need no library symbols.
#[allow(dead_code)]
pub fn front_end_diagnostics(
    src: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Vec<String> {
    front_end_diagnostics_files(&[src], cp_jars, jdk_modules)
}

/// Multi-file form of [`front_end_diagnostics`]. All signatures are collected before every file is
/// checked, matching the module pipeline and keeping cross-file tests on the shared harness.
pub fn front_end_diagnostics_files(
    sources: &[&str],
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Vec<String> {
    front_end_diagnostics_files_with_prepare(sources, cp_jars, jdk_modules, |_, _| {})
}

/// Shared production-shaped diagnostic path. The preparation callback is the only difference
/// between a frontend-only consumer and a backend-aware batch compile; parsing, feature handling,
/// expect/actual stripping, signature inference, checking, and diagnostic deduplication stay in
/// `frontend::analyze_source_set_with_features_and_prepare` instead of being copied into the test
/// harness.
fn front_end_diagnostics_files_with_prepare<F>(
    sources: &[&str],
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
    prepare: F,
) -> Vec<String>
where
    F: FnOnce(&[krusty::ast::File], &mut krusty::frontend::FrontendSymbols),
{
    let cp = cached_classpath(cp_jars, jdk_modules);
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let inputs = sources
        .iter()
        .map(|source| krusty::frontend::SourceInput::kotlin(source))
        .collect::<Vec<_>>();
    let mut diags = krusty::diag::DiagSink::new();
    let _ = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        platform,
        &krusty::features::LangFeatures::new(),
        prepare,
        &mut diags,
    );
    diags.diags.iter().map(|d| d.msg.clone()).collect()
}

/// Multi-file checker diagnostics WITH module facade registration (`prepare_module_symbols`,
/// which the backend drivers run before checking) — cross-file resolution then sees the same
/// positive and negative facade registration as a real compile, unlike
/// [`front_end_diagnostics_files`]. For asserting a cross-file resolution diagnostic. `None`
/// (→ skip) when the toolchain is absent.
#[allow(dead_code)]
pub fn module_front_end_diagnostics(sources: &[(&str, &str)]) -> Option<Vec<String>> {
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    let source_texts = sources
        .iter()
        .map(|(_, source)| *source)
        .collect::<Vec<_>>();
    let stems = sources
        .iter()
        .map(|(stem, _)| (*stem).to_string())
        .collect::<Vec<_>>();
    Some(front_end_diagnostics_files_with_prepare(
        &source_texts,
        &[stdlib],
        Some(jdk.as_path()),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
    ))
}

/// Run a JavaScript source string on Node and return its stdout (trimmed), or `None` if `node` is
/// not on `PATH` (caller skips, exactly like a missing JVM). Used by the `js` backend e2e tests.
#[allow(dead_code)]
pub fn run_js(js: &str) -> Option<String> {
    let node = which_node()?;
    let dir = scratch_dir()?;
    static JS_COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = JS_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("m_{:x}_{n}.mjs", hash_str(js)));
    let stdout_path = path.with_extension("stdout");
    let stderr_path = path.with_extension("stderr");
    std::fs::write(&path, js).ok()?;
    // Enforce the deadline in-process. Depending on GNU `timeout` made every JS test silently skip on
    // macOS even when Node was installed, so local runs never exercised the backend that CI executed.
    // Redirect output to files rather than pipes: a child that fills a pipe would block before
    // `try_wait` observes its exit and turn a successful, verbose program into a false timeout.
    let stdout = std::fs::File::create(&stdout_path).ok()?;
    let stderr = std::fs::File::create(&stderr_path).ok()?;
    let mut child = Command::new(&node)
        .arg(&path)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                timed_out = true;
                break child.wait().ok()?;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(&stdout_path);
                let _ = std::fs::remove_file(&stderr_path);
                return None;
            }
        }
    };
    let stdout = std::fs::read(&stdout_path).unwrap_or_default();
    let stderr = std::fs::read(&stderr_path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&stdout_path);
    let _ = std::fs::remove_file(&stderr_path);
    if timed_out {
        return Some(format!(
            "TIMEOUT:{}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    if !status.success() {
        let code = status.code().unwrap_or(-1);
        return Some(format!(
            "ERROR({code}):{}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    Some(String::from_utf8_lossy(&stdout).trim().to_string())
}

fn which_node() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("KRUSTY_NODE") {
        let p = PathBuf::from(p);
        if node_works(&p) {
            return Some(p);
        }
    }
    for dir in std::env::var("PATH").ok()?.split(':') {
        let cand = Path::new(dir).join("node");
        if node_works(&cand) {
            return Some(cand);
        }
    }
    None
}

fn node_works(path: &Path) -> bool {
    path.exists()
        && Command::new(path)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
}

fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

// The Kotlin toolchain jar location (stdlib family + Maven fallback + JDK modules) lives in the
// library (`krusty::toolchain`) so the box-corpus `survey` binary builds the SAME `-classpath` these
// tests do — one implementation, no drift. Re-exported here under the names the test files already use.
#[allow(unused_imports)]
pub use krusty::toolchain::{
    box_corpus_dir, classpath_jars_for, dist_jar, ensure_maven, find_jar, kotlin_test_jar,
    kotlin_version, kotlinc_lib_dir, stdlib_classpath,
};

// --- Toolchain accessors: a misconfigured environment is a FAILURE, never a skip ---
//
// The library forms report an unprovisioned toolchain as `None`, which is right for the `survey`
// binary but wrong here. An `Option` invites `let Some(x) = … else { return }`, and a whole suite
// then reports as PASSING because nothing ran. These wrappers panic with the reason and the fix, so
// one missing environment variable stops the run instead of hiding it. See
// `tests/box_harness_skip_semantics_e2e.rs` for the sibling rule about compile/run results.

/// The kotlin-stdlib jar every JVM-backed test compiles against.
pub fn stdlib_jar() -> PathBuf {
    krusty::toolchain::stdlib_jar().unwrap_or_else(|| {
        panic!(
            "no kotlin-stdlib jar found, so no JVM-backed test in this suite can compile.\n\
             Provision the reference toolchain (`just` fetches it) or put a kotlin-stdlib jar on the \
             Gradle/Maven cache path."
        )
    })
}

/// `kotlinx-coroutines-core-jvm`, needed by the suspend/`runBlocking` suites.
#[allow(dead_code)]
pub fn coroutines_jar() -> PathBuf {
    krusty::toolchain::coroutines_jar().unwrap_or_else(|| {
        panic!(
            "no kotlinx-coroutines-core jar found, so the coroutine tests cannot compile.\n\
             Provision the reference toolchain (`just`) — it fetches the jar."
        )
    })
}

/// The JDK `lib/modules` jimage the front-end resolves `java.*` against.
///
/// The library form reports an unconfigured toolchain as `None`, which is right for the
/// `survey` binary but wrong here: every JVM-backed test then skips or fails on its own
/// `.expect`, so one missing environment variable surfaces as hundreds of identical
/// panics with nothing naming the cause. Tests stop on the first one instead, with the
/// reason and the fix.
#[allow(dead_code)]
pub fn jdk_modules() -> PathBuf {
    if let Some(modules) = krusty::toolchain::jdk_modules() {
        return modules;
    }
    if let Some(explicit) =
        std::env::var_os("KRUSTY_SURVEY_JDK_MODULES").filter(|path| !path.is_empty())
    {
        panic!(
            "KRUSTY_SURVEY_JDK_MODULES is set but is not a file: {}\n\
             It must point at a JDK's lib/modules jimage. Unset it to fall back to JAVA_HOME.",
            Path::new(&explicit).display()
        );
    }
    // Match `krusty::toolchain::jdk_modules` and Bash's `${JAVA_HOME:-...}` semantics: an
    // exported-but-empty primary variable must not suppress the configured reference JDK.
    let home = std::env::var_os("JAVA_HOME")
        .filter(|home| !home.is_empty())
        .or_else(|| std::env::var_os("KRUSTY_REF_JAVA_HOME").filter(|home| !home.is_empty()))
        .map(PathBuf::from);
    match home {
        None => panic!(
            "JAVA_HOME is not set, so this test cannot resolve java.* and every JVM-backed \
             test in the suite will fail the same way.\n\
             There is no fallback to /usr/libexec/java_home — set it explicitly to a JDK 21+ home."
        ),
        Some(home) => panic!(
            "no lib/modules jimage under JAVA_HOME: {}\n\
             That path is not a JDK home. A package-manager prefix is the usual mistake — the \
             real home is often a subdirectory (e.g. .../openjdk@21/libexec/openjdk.jdk/Contents/Home).",
            home.display()
        ),
    }
}

/// Whether a box-test directive (`// NAME` …) is present. Single source of truth in the lib
/// (`krusty::conformance`), shared with the gate + survey so directive parsing never drifts.
#[allow(dead_code)]
pub fn directive(src: &str, name: &str) -> bool {
    krusty::conformance::directive(src, name)
}

#[allow(dead_code)]
pub fn stdlib_toolchain_ready() -> bool {
    // A probe, so it asks the LIBRARY forms rather than the diagnosing wrappers above: reporting
    // "not ready" is this function's whole job and must not panic.
    krusty::toolchain::stdlib_jar().is_some() && krusty::toolchain::jdk_modules().is_some()
}

/// Run one front-end-valid inline source through the checked-file → JVM-backend pipeline.
///
/// Bail-reason suites deliberately share this helper instead of each rebuilding the checked-file →
/// JVM-backend pipeline. Keeping the classpath and JDK setup here means a diagnostic test differs only
/// in its source and expected reason; it cannot silently drift to a file-, module-, or provider-specific
/// compilation path. As with the surrounding JVM tests, an unavailable provisioned toolchain skips the
/// assertion rather than turning an environment limitation into a compiler failure.
#[allow(dead_code)]
pub fn inline_source_backend_outcome(src: &str) -> Option<BackendOutcome> {
    let jdk = jdk_modules();
    let cp = krusty::toolchain::classpath_jars_for(src);
    backend_outcome_in_process(src, "P", &cp, Some(jdk.as_path()))
}

/// The file must be declined by a BACKEND PASS, naming which one — the counterpart of
/// [`assert_inline_source_lower_bail`] for a gate that lives after lowering.
#[allow(dead_code)]
pub fn assert_inline_source_backend_bail(src: &str, reason: krusty::jvm::backend::SkipReason) {
    if !stdlib_toolchain_ready() {
        return;
    }
    assert_eq!(
        inline_source_backend_outcome(src),
        Some(BackendOutcome::BackendPassBail(reason)),
        "source must stop at its precise unsupported backend-pass boundary:\n{src}"
    );
}

#[allow(dead_code)]
pub fn assert_inline_source_lower_bail(src: &str, reason: &str) {
    if !stdlib_toolchain_ready() {
        return;
    }
    assert_eq!(
        inline_source_backend_outcome(src),
        Some(BackendOutcome::LowerBail(reason.to_string())),
        "source must stop at its precise unsupported lowering boundary:\n{src}"
    );
}

// ---------------------------------------------------------------------------
// Persistent JVM box-runner.
//
// Execution e2e tests used to spawn the krusty binary + `javac` + `java` PER
// TEST (3 process launches, 2 JVM cold-starts each). Under `cargo test` all
// `#[test]`s in a binary share one process, so we instead compile in-process
// (`compile_in_process`) and run `box()` on a PERSISTENT JVM subprocess reused
// across every test — exactly the pattern the conformance gate uses. After
// warmup a test costs ~0 process launches. (Under `nextest`, which runs each
// test in its own process, the pool still avoids `javac` and folds the two
// JVM starts into one.)
// ---------------------------------------------------------------------------

const BOX_RUNNER_SRC: &str = r#"
import java.io.*;
import java.util.concurrent.*;

// Concurrent box-runner. Each request carries an 8-byte id; the main thread only READS requests and
// hands each to a worker pool, so many box() calls run in parallel (a single test binary with N test
// threads keeps N calls in flight). Responses are written back tagged with their id — possibly out of
// order — under a lock on the output stream. The Rust client demuxes replies by id.
public class BoxRunner {
    static final long TIMEOUT_MS = 10000;
    static final ExecutorService EXEC = Executors.newCachedThreadPool(r -> {
        Thread t = new Thread(r);
        t.setDaemon(true);
        return t;
    });

    public static void main(String[] args) throws Exception {
        DataInputStream din = new DataInputStream(new BufferedInputStream(System.in, 65536));
        final DataOutputStream dout = new DataOutputStream(new BufferedOutputStream(System.out, 4096));
        System.setOut(System.err);
        while (true) {
            long id;
            try { id = din.readLong(); } catch (EOFException e) { break; }
            int n = din.readInt();
            String[] names = new String[n];
            byte[][] data = new byte[n][];
            for (int i = 0; i < n; i++) {
                int nl = din.readUnsignedShort();
                names[i] = new String(din.readNBytes(nl), "UTF-8");
                int dl = din.readInt();
                data[i] = din.readNBytes(dl);
            }
            int bl = din.readUnsignedShort();
            String boxClass = new String(din.readNBytes(bl), "UTF-8");
            final long idF = id;
            final String[] namesF = names;
            final byte[][] dataF = data;
            final String boxClassF = boxClass;
            // Each request runs on its own worker so the read loop never blocks. The inner future
            // bounds a single box() call's wall time without stalling other in-flight requests.
            EXEC.submit(() -> {
                String result;
                Future<String> future = EXEC.submit(() -> {
                    try {
                        TestClassLoader ldr = new TestClassLoader(namesF, dataF);
                        Class<?> cls = ldr.loadClass(boxClassF);
                        String r = (String) cls.getMethod("box").invoke(null);
                        return r == null ? "null" : r;
                    } catch (Throwable t) {
                        Throwable cause = (t instanceof java.lang.reflect.InvocationTargetException && t.getCause() != null) ? t.getCause() : t;
                        StringBuilder detail = new StringBuilder();
                        for (Throwable current = cause; current != null; current = current.getCause()) {
                            if (detail.length() > 0) detail.append(" <- ");
                            detail.append(current.getClass().getSimpleName()).append(":").append(current.getMessage());
                        }
                        return "ERROR:" + detail;
                    }
                });
                try {
                    result = future.get(TIMEOUT_MS, TimeUnit.MILLISECONDS);
                } catch (TimeoutException e) {
                    future.cancel(true);
                    result = "ERROR:TimeoutException:box() exceeded " + TIMEOUT_MS + "ms";
                } catch (Throwable e) {
                    Throwable c = e instanceof ExecutionException && e.getCause() != null ? e.getCause() : e;
                    result = "ERROR:" + c.getClass().getSimpleName() + ":" + c.getMessage();
                }
                byte[] rb;
                try { rb = result.getBytes("UTF-8"); } catch (Exception e) { rb = new byte[0]; }
                synchronized (dout) {
                    try {
                        dout.writeLong(idF);
                        dout.writeInt(rb.length);
                        dout.write(rb);
                        dout.flush();
                    } catch (IOException e) { /* client gone */ }
                }
            });
        }
    }
}

class TestClassLoader extends ClassLoader {
    private java.util.HashMap<String, byte[]> classes = new java.util.HashMap<>();
    TestClassLoader(String[] names, byte[][] data) {
        super(ClassLoader.getSystemClassLoader());
        for (int i = 0; i < names.length; i++)
            classes.put(names[i].replace('/', '.'), data[i]);
    }
    @Override protected Class<?> findClass(String name) throws ClassNotFoundException {
        byte[] b = classes.get(name);
        if (b != null) return defineClass(name, b, 0, b.length);
        throw new ClassNotFoundException(name);
    }
}
"#;

/// Locate `JAVA_HOME` for the runner JVM (`KRUSTY_REF_JAVA_HOME` overrides). `None` ⇒ skip.
#[allow(dead_code)]
pub fn java_home() -> String {
    std::env::var("KRUSTY_REF_JAVA_HOME")
        .or_else(|_| std::env::var("JAVA_HOME"))
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "JAVA_HOME is not set, so no JVM-backed test in this suite can run.\n\
                 There is no fallback to /usr/libexec/java_home — set it explicitly to a JDK 21+ home."
            )
        })
}

/// Compile `BoxRunner.java` once into a stable cache dir keyed by the source hash; return its dir.
fn setup_runner(java_home: &str) -> Option<PathBuf> {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in BOX_RUNNER_SRC.bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/box_runner_{hash:016x}"));
    if dir.join("BoxRunner.class").is_file() {
        return Some(dir);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let src_path = dir.join("BoxRunner.java");
    std::fs::write(&src_path, BOX_RUNNER_SRC).ok()?;
    let javac = format!("{java_home}/bin/javac");
    if !Path::new(&javac).exists() {
        return None;
    }
    let out = Command::new(&javac)
        .args(["-source", "8", "-target", "8", "-d", dir.to_str().unwrap()])
        .arg(&src_path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(dir)
}

/// A persistent JVM subprocess that runs `box()` calls CONCURRENTLY. Requests are tagged with an id
/// and written under a short stdin lock; a background reader thread demuxes tagged responses back to
/// the waiting caller by id. Many threads can therefore have box() calls in flight at once (bounded
/// only by the JVM worker pool), so a multi-threaded test binary overlaps its JVM round-trips instead
/// of serialising on one lock.
struct BoxRunner {
    child: Child,
    stdin: Mutex<ChildStdin>,
    waiters: Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>>,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
}

impl Drop for BoxRunner {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Opt-in JVM diagnostics for a runner: set `KRUSTY_JVM_GCLOG=<dir>` to make every runner JVM log GC,
/// metaspace, and class-unload events to `<dir>/<role>-<pid>.gclog` (unified `-Xlog`). Empty ⇒ no args,
/// zero overhead. Lets a profiling run see GC frequency/pauses and whether metaspace/class-unloading is
/// a bottleneck, instead of guessing. Pair with `KRUSTY_PROF=1` for the per-phase (compile/box) timings.
#[allow(dead_code)]
fn jvm_gclog_args(role: &str) -> Vec<String> {
    let Some(dir) = std::env::var("KRUSTY_JVM_GCLOG")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        return Vec::new();
    };
    let _ = std::fs::create_dir_all(&dir);
    let pid = std::process::id();
    vec![format!(
        "-Xlog:gc*=info,gc+metaspace=info,class+unload=info:file={dir}/{role}-{pid}.gclog:time,uptime,level,tags"
    )]
}

impl BoxRunner {
    fn new(java: &str, cp: &str) -> Option<Self> {
        let mut cmd = Command::new(java);
        cmd.args(jvm_gclog_args("boxrunner"));
        // Cap the runner heap (-Xmx512m) to keep each persistent runner small — they used to grow to
        // ~1 GB and several run at once, so this eases gate memory pressure. Deliberately keep the
        // DEFAULT collector + full tiered JIT: BoxRunner dispatches box() bodies on a cached thread pool
        // (they run concurrently), so a single-threaded stop-the-world collector (-XX:+UseSerialGC) or
        // C1-only JIT (-XX:TieredStopAtLevel=1) throttles the whole pool at every GC / starves hot loops
        // — that alone ballooned the suite from minutes to hours. The heap cap bounds footprint; G1 keeps
        // throughput.
        cmd.args(["-Xmx512m", "-cp", cp, "BoxRunner"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        // Under KRUSTY_JVM_GCLOG, keep the runner's stderr (a crash/OOM reason) instead of discarding it.
        match std::env::var("KRUSTY_JVM_GCLOG")
            .ok()
            .filter(|v| !v.is_empty())
        {
            Some(dir) => match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(format!("{dir}/boxrunner-{}.stderr", std::process::id()))
            {
                Ok(f) => {
                    cmd.stderr(Stdio::from(f));
                }
                Err(_) => {
                    cmd.stderr(Stdio::null());
                }
            },
            None => {
                cmd.stderr(Stdio::null());
            }
        }
        die_with_parent(&mut cmd);
        let mut child = spawn_owned(cmd).ok()?;
        let stdin = child.stdin.take()?;
        let mut stdout = child.stdout.take()?;
        let waiters: Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let w2 = Arc::clone(&waiters);
        let a2 = Arc::clone(&alive);
        // Reader thread: pull tagged responses off the JVM's stdout and hand each to its waiter. On
        // EOF/error (JVM died) mark dead and drop every waiter's sender so blocked callers wake with
        // an error and the runner gets restarted.
        std::thread::spawn(move || {
            let mut hdr = [0u8; 12];
            loop {
                if stdout.read_exact(&mut hdr).is_err() {
                    break;
                }
                let id = u64::from_be_bytes(hdr[0..8].try_into().unwrap());
                let len = u32::from_be_bytes(hdr[8..12].try_into().unwrap()) as usize;
                let mut body = vec![0u8; len];
                if stdout.read_exact(&mut body).is_err() {
                    break;
                }
                if let Some(tx) = w2.lock().unwrap().remove(&id) {
                    let _ = tx.send(body);
                }
            }
            a2.store(false, Ordering::SeqCst);
            w2.lock().unwrap().clear();
        });
        Some(BoxRunner {
            child,
            stdin: Mutex::new(stdin),
            waiters,
            next_id: AtomicU64::new(1),
            alive,
        })
    }

    fn try_run(&self, classes: &[(String, Vec<u8>)], box_class: &str) -> std::io::Result<String> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "BoxRunner dead",
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.waiters.lock().unwrap().insert(id, tx);

        // Frame the whole request into one buffer, then write it under the stdin lock so concurrent
        // requests never interleave on the pipe.
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&id.to_be_bytes());
        buf.extend_from_slice(&(classes.len() as u32).to_be_bytes());
        for (name, data) in classes {
            buf.extend_from_slice(&(name.len() as u16).to_be_bytes());
            buf.extend_from_slice(name.as_bytes());
            buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
            buf.extend_from_slice(data);
        }
        buf.extend_from_slice(&(box_class.len() as u16).to_be_bytes());
        buf.extend_from_slice(box_class.as_bytes());
        {
            let mut stdin = self.stdin.lock().unwrap();
            stdin.write_all(&buf)?;
            stdin.flush()?;
        }

        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(body) => Ok(String::from_utf8_lossy(&body).into_owned()),
            Err(_) => {
                self.waiters.lock().unwrap().remove(&id);
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "BoxRunner timeout",
                ))
            }
        }
    }
}

/// Blocking `read_exact` with a wall-clock deadline (via `poll`), for the request/response JVM servers
/// (`KotlincServer` et al.) whose one-pipe protocol reads directly rather than through a demux thread.
fn read_exact_deadline(fd: i32, buf: &mut [u8], deadline: Instant) -> std::io::Result<()> {
    let mut pos = 0;
    while pos < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "read timeout",
            ));
        }
        let poll_ms = remaining.as_millis().min(1000) as i32;
        let ready = unsafe {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            libc::poll(&mut pfd, 1, poll_ms) > 0 && (pfd.revents & libc::POLLIN != 0)
        };
        if !ready {
            continue;
        }
        let nread = unsafe {
            libc::read(
                fd,
                buf[pos..].as_mut_ptr() as *mut libc::c_void,
                (buf.len() - pos) as libc::size_t,
            )
        };
        match nread {
            -1 => {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            0 => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "EOF",
                ));
            }
            n => pos += n as usize,
        }
    }
    Ok(())
}

/// Find the class declaring `static box()Ljava/lang/String;` among compiled classes.
#[allow(dead_code)]
pub fn find_box_class(classes: &[(String, Vec<u8>)]) -> Option<String> {
    for (name, bytes) in classes {
        if let Ok(ci) = krusty::jvm::classreader::parse_class(bytes) {
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

struct RunnerPool {
    runners: HashMap<String, Arc<BoxRunner>>,
    order: VecDeque<String>,
}

impl RunnerPool {
    fn new() -> Self {
        RunnerPool {
            runners: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn touch(&mut self, cp: &str) {
        self.order.retain(|k| k != cp);
        self.order.push_back(cp.to_string());
    }

    fn insert(&mut self, cp: String, runner: Arc<BoxRunner>) {
        self.runners.insert(cp.clone(), runner);
        self.touch(&cp);
        self.prune();
    }

    fn get(&mut self, cp: &str) -> Option<Arc<BoxRunner>> {
        let runner = self.runners.get(cp).cloned();
        if runner.is_some() {
            self.touch(cp);
        }
        runner
    }

    fn remove(&mut self, cp: &str) {
        self.runners.remove(cp);
        self.order.retain(|k| k != cp);
    }

    fn prune(&mut self) {
        self.runners.retain(|_, r| r.alive.load(Ordering::SeqCst));
        self.order.retain(|k| self.runners.contains_key(k));
        let max = std::env::var("KRUSTY_BOX_RUNNER_POOL")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(2);
        while self.runners.len() > max {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            let removable = self
                .runners
                .get(&old)
                .is_some_and(|r| Arc::strong_count(r) == 1);
            if removable {
                self.runners.remove(&old);
            } else {
                self.order.push_back(old);
                break;
            }
        }
    }
}

/// `(internal_name, bytes)` pairs, the shape BoxRunner's in-memory classloader consumes.
#[allow(dead_code)]
type ClassSet = Vec<(String, Vec<u8>)>;

/// Recursively collect `(internal_name, bytes)` for every `.class` under a directory classpath
/// entry, memoized by directory path. Safe to memoize: cached lib dirs are immutable once published
/// (`compile_libs`), and per-test scratch dirs are unique per allocation, so a path's contents never
/// change between calls within one process.
#[allow(dead_code)]
fn dir_classes(dir: &Path) -> Option<Arc<ClassSet>> {
    type DirClassesMemo = Mutex<HashMap<PathBuf, Arc<ClassSet>>>;
    static MEMO: OnceLock<DirClassesMemo> = OnceLock::new();
    let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = memo
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(dir)
        .cloned()
    {
        return Some(hit);
    }
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Option<()> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out)?;
            } else if path.extension().is_some_and(|e| e == "class") {
                let rel = path.strip_prefix(root).ok()?;
                let name = rel
                    .to_string_lossy()
                    .trim_end_matches(".class")
                    .replace(std::path::MAIN_SEPARATOR, "/");
                out.push((name, std::fs::read(&path).ok()?));
            }
        }
        Some(())
    }
    let mut classes = Vec::new();
    walk(dir, dir, &mut classes)?;
    let arc = Arc::new(classes);
    memo.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dir.to_path_buf(), arc.clone());
    Some(arc)
}

/// Run `box()` on already-compiled classes via a persistent JVM keyed by `cp_jars` (the runtime
/// classpath — typically the stdlib jar so loaded classes resolve `kotlin.jvm.internal.*`). Returns
/// the `box()` return value (or `ERROR:…`), or `None` if the JVM environment is unavailable.
///
/// The runner is concurrency-safe (id-tagged requests, demuxed responses), so callers on different
/// threads share one JVM without an exclusive round-trip lock.
#[allow(dead_code)]
pub fn run_box(
    classes: &[(String, Vec<u8>)],
    box_class: &str,
    cp_jars: &[PathBuf],
) -> Option<String> {
    static POOL: OnceLock<Mutex<RunnerPool>> = OnceLock::new();
    let _pg = ProfGuard::new("box");
    let java_home = java_home();
    let java = format!("{java_home}/bin/java");
    if !Path::new(&java).exists() {
        return None;
    }
    let runner_dir = setup_runner(&java_home)?;
    // DIRECTORY classpath entries (kotlinc-built dependency libs) are shipped as bytes into the
    // per-request TestClassLoader instead of onto the runner JVM's system classpath. Two reasons:
    // the runner is keyed by its classpath string, so per-lib dirs would spawn a runner JVM per lib
    // (churn), and system-classpath classes are initialized ONCE per JVM, so a box() that mutates a
    // lib object's static state would poison every later box() sharing that (now cached) lib. Jars
    // (stdlib, reflect, coroutines) stay on the system classpath: they are large, shared, and tests
    // don't assert on their mutable static state.
    let mut request_classes: Vec<(String, Vec<u8>)> = Vec::new();
    let mut cp = runner_dir.to_string_lossy().into_owned();
    for j in cp_jars {
        if j.is_dir() {
            request_classes.extend(dir_classes(j)?.iter().cloned());
        } else {
            cp.push(':');
            cp.push_str(&j.to_string_lossy());
        }
    }
    let classes = if request_classes.is_empty() {
        std::borrow::Cow::Borrowed(classes)
    } else {
        request_classes.extend(classes.iter().cloned());
        std::borrow::Cow::Owned(request_classes)
    };
    let classes: &[(String, Vec<u8>)] = &classes;
    let pool = POOL.get_or_init(|| {
        sweep_stale_temp_dirs();
        Mutex::new(RunnerPool::new())
    });

    // Fetch (or spin up) the runner for this classpath under the pool lock, then release the lock so
    // the actual round-trip runs concurrently with other threads' calls. The pool is capped because a
    // single grouped e2e binary can see many short-lived temp classpaths; without recycling, each one
    // leaves behind a persistent JVM for the rest of the process.
    let get_runner = || -> Option<Arc<BoxRunner>> {
        let mut map = pool.lock().unwrap();
        let needs_runner = map.get(&cp).is_none_or(|r| !r.alive.load(Ordering::SeqCst));
        if needs_runner {
            map.insert(cp.clone(), Arc::new(BoxRunner::new(&java, &cp)?));
        }
        map.get(&cp)
    };

    let runner = get_runner()?;
    match runner.try_run(classes, box_class) {
        Ok(s) => Some(s),
        Err(_) => {
            // The JVM died or timed out. Replace the dead runner (if another thread hasn't already)
            // and retry once on a fresh one.
            {
                let mut map = pool.lock().unwrap();
                if map
                    .runners
                    .get(&cp)
                    .is_some_and(|r| Arc::ptr_eq(r, &runner))
                {
                    map.remove(&cp);
                }
            }
            let fresh = get_runner()?;
            fresh.try_run(classes, box_class).ok()
        }
    }
}

/// Compile `src` in-process and run `box()` on the persistent JVM. `cp_jars` is BOTH the compile
/// classpath and the runtime classpath; `jdk_modules` is the compile-time bootclasspath jimage.
/// Returns the `box()` result, or `None` if compilation failed or the JVM is unavailable.
#[allow(dead_code)]
pub fn compile_and_run_box(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&Path>,
) -> Option<String> {
    let classes = compile_in_process(src, stem, cp_jars, jdk_modules)?;
    let box_class = find_box_class(&classes)?;
    run_box(&classes, &box_class, cp_jars)
}

/// Compile a same-module source set and run its `box()` entry point.
#[allow(dead_code)]
pub fn compile_and_run_box_files(
    sources: &[(&str, &str)],
    cp_jars: &[PathBuf],
    jdk_modules: Option<&Path>,
) -> Option<String> {
    let classes = compile_in_process_files(sources, cp_jars, jdk_modules)?;
    let box_class = find_box_class(&classes)?;
    run_box(&classes, &box_class, cp_jars)
}

/// A POSITIVE front-end coverage test upgraded to TRUE e2e: the source must be checker-clean, the
/// BACKEND must emit it (a lowering/emit bail is a failure, not a skip), and when it declares
/// `fun box()`, running it must return "OK". Use for tests whose old form only asserted empty
/// diagnostics — the front end accepting a shape means nothing if the backend can't realize it.
/// `extra_cp` joins kotlin-stdlib + the JDK on the classpath (empty for stdlib-only sources).
#[allow(dead_code)]
pub fn expect_true_e2e(tag: &str, src: &str, extra_cp: &[PathBuf]) {
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    let mut cp = extra_cp.to_vec();
    cp.push(stdlib);
    let diagnostics = front_end_diagnostics(src, &cp, Some(jdk.as_path()));
    assert!(
        diagnostics.is_empty(),
        "{tag}: expected a checker-clean source, got: {diagnostics:?}"
    );
    let Some(classes) = compile_in_process(src, "Main", &cp, Some(jdk.as_path())) else {
        panic!("{tag}: the front end accepted the source but the backend bailed on emitting it");
    };
    if let Some(box_class) = find_box_class(&classes) {
        let out = run_box(&classes, &box_class, &cp)
            .unwrap_or_else(|| panic!("{tag}: emitted classes but the box() run failed to start"));
        assert!(
            !out.trim().starts_with("ERROR:"),
            "{tag}: box() threw: {out}"
        );
        // Only a fixture written for the convention is held to it — some upgraded checker tests
        // return a domain value ("RED") rather than "OK".
        if src.contains("\"OK\"") {
            assert_eq!(out.trim(), "OK", "{tag}: box() returned {out:?}");
        }
    }
}

/// Compile `src` with kotlin-stdlib plus the provisioned JDK modules, then run `box()`.
#[allow(dead_code)]
pub fn compile_and_run_with_stdlib(src: &str, stem: &str) -> Option<String> {
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    compile_and_run_box(src, stem, &[stdlib], Some(jdk.as_path()))
}

/// Multi-file form of [`compile_and_run_with_stdlib`].
#[allow(dead_code)]
pub fn compile_and_run_files_with_stdlib(sources: &[(&str, &str)]) -> Option<String> {
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    compile_and_run_box_files(sources, &[stdlib], Some(jdk.as_path()))
}

/// Render the front-end diagnostics behind a `None` from the compile helpers. Empty means the front
/// end ACCEPTED the source, so the `None` came from a lowering/emit bail instead.
fn why(diagnostics: &[String]) -> String {
    if diagnostics.is_empty() {
        "the front end accepted it, so lowering/emit bailed".to_string()
    } else {
        format!("front-end diagnostics: {diagnostics:?}")
    }
}

/// Compile `src` and run `box()`, treating a `None` as a REGRESSION rather than a skip.
///
/// [`compile_and_run_box`] returns `None` for two unrelated reasons — the JVM toolchain isn't
/// provisioned, or the front end/backend REJECTED the source. A caller that has already resolved the
/// toolchain (stdlib jar, JDK modules, JVM runner) has ruled the first out, so the remaining causes
/// are a compile failure or a lowering/emit bail: a real failure that must not report as a pass.
/// Panics with the front-end diagnostics (empty ⇒ the front end accepted it and lowering/emit bailed).
#[allow(dead_code)]
pub fn expect_box_run(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&Path>,
) -> String {
    compile_and_run_box(src, stem, cp_jars, jdk_modules).unwrap_or_else(|| {
        let diagnostics = front_end_diagnostics(src, cp_jars, jdk_modules);
        let backend = diagnostics
            .is_empty()
            .then(|| backend_outcome_in_process(src, stem, cp_jars, jdk_modules));
        panic!(
            "{stem}: compile/run returned None; {}; backend outcome: {backend:?}",
            why(&diagnostics)
        )
    })
}

/// [`expect_box_run`] against kotlin-stdlib + the JDK modules.
///
/// This is deliberately fail-fast rather than optional: both toolchain accessors panic when their
/// inputs are unavailable, and [`expect_box_run`] panics with diagnostics when compilation or
/// execution fails. Encoding an impossible `None` invited callers to retain dead skip branches that
/// could make a future regression look like a passing test.
#[allow(dead_code)]
pub fn expect_box_run_with_stdlib(src: &str, stem: &str) -> String {
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    expect_box_run(src, stem, &[stdlib], Some(jdk.as_path()))
}

/// [`expect_box_run`] for a compile-only consumer: the emitted classes, or a panic naming why the
/// source was rejected. Same contract — the caller must have resolved the toolchain first.
#[allow(dead_code)]
pub fn expect_compile_in_process(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&Path>,
) -> Vec<(String, Vec<u8>)> {
    compile_in_process(src, stem, cp_jars, jdk_modules).unwrap_or_else(|| {
        let diagnostics = front_end_diagnostics(src, cp_jars, jdk_modules);
        panic!("{stem}: compile returned None; {}", why(&diagnostics))
    })
}

/// [`expect_compile_in_process`] against kotlin-stdlib + the JDK modules, with the same fail-fast
/// contract as [`expect_box_run_with_stdlib`].
#[allow(dead_code)]
pub fn expect_classes_with_stdlib(src: &str, stem: &str) -> Vec<(String, Vec<u8>)> {
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    expect_compile_in_process(src, stem, &[stdlib], Some(jdk.as_path()))
}

/// Compile `src` with kotlin-stdlib + JDK modules, run `box()`, and assert it returns `OK`. Once
/// stdlib + JDK modules are provisioned a compile/run `None` is a regression, not a skip.
#[allow(dead_code)]
pub fn expect_box_ok_with_stdlib(src: &str, stem: &str) {
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    assert_eq!(
        expect_box_run(src, stem, &[stdlib], Some(jdk.as_path())),
        "OK",
        "{stem}"
    );
}

/// Multi-file form of [`expect_box_ok_with_stdlib`].
#[allow(dead_code)]
pub fn expect_box_ok_files_with_stdlib(sources: &[(&str, &str)], stem: &str) {
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    let cp = [stdlib];
    let out = compile_and_run_box_files(sources, &cp, Some(jdk.as_path())).unwrap_or_else(|| {
        let files: Vec<&str> = sources.iter().map(|(_, src)| *src).collect();
        let diagnostics = front_end_diagnostics_files(&files, &cp, Some(jdk.as_path()));
        panic!("{stem}: compile/run returned None; {}", why(&diagnostics))
    });
    assert_eq!(out, "OK", "{stem}");
}

/// Assert that a multi-file source set is accepted by the shared frontend with stdlib/JDK symbols.
#[allow(dead_code)]
pub fn expect_front_end_ok_files_with_stdlib(sources: &[&str], stem: &str) {
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    let diagnostics =
        front_end_diagnostics_files(sources, std::slice::from_ref(&stdlib), Some(jdk.as_path()));
    assert!(
        diagnostics.is_empty(),
        "{stem}: unexpected diagnostics: {diagnostics:?}"
    );
}

/// Compile Kotlin source into a temporary classpath directory.
/// Returns `None` when the Kotlin toolchain is unavailable.
#[allow(dead_code)]
pub fn compile_lib(tag: &str, lib_src: &str) -> Option<PathBuf> {
    compile_libs(tag, &[("Lib.kt", lib_src)])
}

/// Compile Kotlin files into a classpath directory, memoized per source set for this run only.
///
/// The lib is compiled BY KRUSTY, in-process (~tens of ms) — the product compiles its own test
/// dependencies, which also exercises krusty's classfile/`@Metadata` EMISSION on every lib fixture.
/// A lib krusty cannot build FAILS the test with krusty's diagnostics — there is deliberately no
/// reference-compiler fallback and no silent skip: a dependency gap is a product gap and must be
/// visible. The reference kotlinc appears through the DEFAULT-ON cross-check (disable explicitly
/// with `KRUSTY_LIB_CROSSCHECK=0`), which compiles every lib with BOTH compilers and asserts the
/// same `box()` result against each (see [`LibBuild::cross_check_box`]). There is NO on-disk cache of compiled
/// dependencies — only a per-run memo, so every run rebuilds its deps with the compiler under
/// test. The memo also keeps the returned PATH stable within a run: `run_box` keys its persistent
/// runner JVMs by classpath, so per-call scratch dirs would spawn a runner JVM per test.
#[allow(dead_code)]
pub fn compile_libs(tag: &str, sources: &[(&str, &str)]) -> Option<PathBuf> {
    compile_libs_build(tag, sources).map(|b| b.krusty_out().to_path_buf())
}

/// One dependency build: the krusty-built classpath dir, plus a lazily kotlinc-built reference dir
/// used only by the opt-in cross-check. Memoized per run and per source set.
#[allow(dead_code)]
pub struct LibBuild {
    sources: Vec<(String, String)>,
    krusty: PathBuf,
    reference: OnceLock<Option<PathBuf>>,
}

#[allow(dead_code)]
impl LibBuild {
    /// The krusty-built classpath dir tests consume.
    pub fn krusty_out(&self) -> &Path {
        &self.krusty
    }

    /// The kotlinc-built reference dir, compiled lazily on first request (a default run never pays
    /// a reference compile). `None` when kotlinc is unavailable. Panics when kotlinc REJECTS the
    /// sources — krusty accepted invalid Kotlin, which the cross-check exists to catch.
    pub fn reference_out(&self) -> Option<&Path> {
        self.reference
            .get_or_init(|| {
                let sources: Vec<(&str, &str)> = self
                    .sources
                    .iter()
                    .map(|(n, s)| (n.as_str(), s.as_str()))
                    .collect();
                kotlinc_lib_out(&sources)
            })
            .as_deref()
    }

    /// Default-on differential check (opt out: `KRUSTY_LIB_CROSSCHECK=0`): the same `main`, run
    /// against the kotlinc-built lib, must produce the same `box()` result the krusty-built lib
    /// produced. A `main` that compiles against the krusty-built dependency but NOT against the
    /// reference one means krusty's emitted metadata invented surface — that fails hard too.
    fn cross_check_box(&self, tag: &str, main: &str, extra_cp: &[PathBuf], krusty_result: &str) {
        if !lib_crosscheck_enabled() {
            return;
        }
        let Some(reference) = self.reference_out() else {
            return; // no kotlinc provisioned — nothing to compare against
        };
        if std::env::var("KRUSTY_LIB_BYTEDIFF_REPORT").is_ok() {
            let mut kmap = std::collections::BTreeMap::new();
            collect_rel_files(&self.krusty, &self.krusty, &mut kmap);
            let mut rmap = std::collections::BTreeMap::new();
            collect_rel_files(reference, reference, &mut rmap);
            for (name, bytes) in &kmap {
                match rmap.get(name) {
                    Some(rb) if rb == bytes => eprintln!("LIBDIFF\tidentical\t{name}\t{tag}"),
                    Some(_) => eprintln!("LIBDIFF\tdivergent\t{name}\t{tag}"),
                    None => eprintln!("LIBDIFF\tkrusty-only\t{name}\t{tag}"),
                }
            }
            for name in rmap.keys() {
                if !kmap.contains_key(name) {
                    eprintln!("LIBDIFF\tkotlinc-only\t{name}\t{tag}");
                }
            }
        }
        let jdk = jdk_modules();
        let mut cp = vec![reference.to_path_buf()];
        cp.extend_from_slice(extra_cp);
        let Some(classes) = compile_in_process(main, "Main", &cp, Some(jdk.as_path())) else {
            panic!(
                "{tag}: main compiles against the krusty-built dependency but not against the \
                 kotlinc-built one — krusty's emitted lib metadata declares surface kotlinc's \
                 does not"
            );
        };
        let Some(box_class) = find_box_class(&classes) else {
            return;
        };
        let Some(got) = run_box(&classes, &box_class, &cp) else {
            return;
        };
        assert_eq!(
            got, krusty_result,
            "{tag}: box() diverged between the krusty-built and kotlinc-built dependency"
        );
    }
}

#[allow(dead_code)]
fn collect_rel_files(
    root: &Path,
    dir: &Path,
    out: &mut std::collections::BTreeMap<String, Vec<u8>>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_rel_files(root, &p, out);
        } else if let (Ok(rel), Ok(bytes)) = (p.strip_prefix(root), std::fs::read(&p)) {
            out.insert(rel.to_string_lossy().into_owned(), bytes);
        }
    }
}

/// The both-compilers differential for dependency libs is ON BY DEFAULT — every krusty-built lib
/// is also built with the reference kotlinc and the same `main` must produce the same `box()`
/// result against both. Disable EXPLICITLY with `KRUSTY_LIB_CROSSCHECK=0` (e.g. a fast local loop
/// or a host with no kotlinc dist); any other value, or unset, keeps it on.
#[allow(dead_code)]
fn lib_crosscheck_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("KRUSTY_LIB_CROSSCHECK").as_deref(),
            Ok("0") | Ok("false") | Ok("off")
        )
    })
}

/// Build (or fetch this run's memoized build of) a dependency source set. Panics — with krusty's
/// own diagnostics — when krusty cannot compile the sources; never falls back, never skips. `None`
/// is reserved for a broken scratch filesystem. See [`compile_libs`].
#[allow(dead_code)]
pub fn compile_libs_build(tag: &str, sources: &[(&str, &str)]) -> Option<Arc<LibBuild>> {
    // Per-run memo + coalescing: concurrent tests asking for the same source set block on one
    // build (OnceLock::get_or_init) instead of racing N identical compiles.
    type LibMemo = Mutex<HashMap<u64, Arc<OnceLock<Option<Arc<LibBuild>>>>>>;
    static MEMO: OnceLock<LibMemo> = OnceLock::new();

    let mut hash: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
        }
        hash = (hash ^ 0xff).wrapping_mul(0x100000001b3); // field separator
    };
    for (name, src) in sources {
        feed(name.as_bytes());
        feed(src.as_bytes());
    }

    let cell = {
        let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = memo.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(hash).or_default().clone()
    };
    cell.get_or_init(|| {
        let krusty = match krusty_lib_out(sources) {
            Ok(out) => out?,
            Err(diags) => panic!(
                "{tag}: krusty failed to compile this test's dependency lib — a product gap, \
                 surfaced as a failure by design (no reference-compiler fallback).\n{diags}"
            ),
        };
        Some(Arc::new(LibBuild {
            sources: sources
                .iter()
                .map(|(n, s)| ((*n).to_string(), (*s).to_string()))
                .collect(),
            krusty,
            reference: OnceLock::new(),
        }))
    })
    .clone()
}

/// Compile a dependency source set with KRUSTY, in-process, through the module-wide driver (the
/// same pipeline `krusty -d` uses), writing every output — classes AND the `.kotlin_module` facade
/// index — verbatim into a scratch classpath dir. `Err` carries the diagnostics (or the bail
/// shape) so the caller can fail the test descriptively; `Ok(None)` = scratch dir unavailable.
#[allow(dead_code)]
fn krusty_lib_out(sources: &[(&str, &str)]) -> Result<Option<PathBuf>, String> {
    use krusty::diag::DiagSink;
    use krusty::frontend::collect_signatures_with_cp;

    let _pg = ProfGuard::new("krusty_lib");
    let mut diags = DiagSink::new();
    let texts: Vec<&str> = sources.iter().map(|(_, s)| *s).collect();
    let render = |diags: &krusty::diag::DiagSink| {
        let named: Vec<(&str, &str)> = sources.iter().map(|(n, s)| (*n, *s)).collect();
        diags.render_all(&named)
    };
    let Some(files) = parse_source_set(&texts, &mut diags) else {
        return Err(render(&diags));
    };
    let jdk = krusty::toolchain::jdk_modules();
    let cp = cached_classpath(&[stdlib_jar()], jdk.as_deref());
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let mut symbols = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return Err(render(&diags));
    }
    let stems: Vec<String> = sources
        .iter()
        .map(|(name, _)| name.trim_end_matches(".kt").to_string())
        .collect();
    krusty::jvm::prepare_module_symbols(&files, &stems, &mut symbols);
    let backend = krusty::jvm::JvmBackend::new(cp);
    let outputs =
        krusty::compiler::compile(&files, &stems, &mut symbols, &backend, "main", &mut diags);
    if diags.has_errors() {
        return Err(render(&diags));
    }
    if outputs.is_empty() {
        return Err("backend produced no classes (bail) for the dependency lib".to_string());
    }
    let Some(scratch) = scratch_dir() else {
        return Ok(None);
    };
    let out = scratch.join("klibout");
    for (path, bytes) in &outputs {
        let dest = out.join(path);
        if let Some(parent) = dest.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return Ok(None);
            }
        }
        if std::fs::write(dest, bytes).is_err() {
            return Ok(None);
        }
    }
    Ok(Some(out))
}

/// EXPLICITLY reference-compiled dependency: for tests whose contract is krusty CONSUMING
/// kotlinc-emitted classfiles/`@Metadata` — shapes krusty's own emitter doesn't produce (yet or by
/// design). This is a per-test-site declaration, not a fallback: the default [`compile_lib`] is
/// krusty-built, and a test spelled `_ref` documents that its dependency MUST come from the
/// reference compiler. Memoized per run like the krusty builds. `None` = kotlinc unavailable.
#[allow(dead_code)]
pub fn compile_lib_ref(tag: &str, lib_src: &str) -> Option<PathBuf> {
    compile_libs_ref(tag, &[("Lib.kt", lib_src)])
}

/// Multi-file form of [`compile_lib_ref`].
#[allow(dead_code)]
pub fn compile_libs_ref(_tag: &str, sources: &[(&str, &str)]) -> Option<PathBuf> {
    // Diagnostic mode (KRUSTY_REF_SELF=1): route the reference-compiled dependency through the
    // krusty build instead, to measure which `_ref` sites can already flip back to the default.
    if std::env::var("KRUSTY_REF_SELF").as_deref() == Ok("1") {
        return compile_libs(_tag, sources);
    }
    type RefMemo = Mutex<HashMap<u64, Arc<OnceLock<Option<PathBuf>>>>>;
    static MEMO: OnceLock<RefMemo> = OnceLock::new();

    let mut hash: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
        }
        hash = (hash ^ 0xff).wrapping_mul(0x100000001b3);
    };
    for (name, src) in sources {
        feed(name.as_bytes());
        feed(src.as_bytes());
    }
    let cell = {
        let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = memo.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(hash).or_default().clone()
    };
    cell.get_or_init(|| kotlinc_lib_out(sources)).clone()
}

/// [`run_box_against`] with an EXPLICITLY reference-compiled dependency (see [`compile_lib_ref`]).
#[allow(dead_code)]
pub fn run_box_against_ref(tag: &str, lib_src: &str, main: &str) -> Option<String> {
    let libout = compile_lib_ref(tag, lib_src)?;
    let stdlib = stdlib_jar();
    compile_and_run_box(
        main,
        "Main",
        &[libout, stdlib],
        Some(jdk_modules().as_path()),
    )
}

/// [`expect_box_run_against`] with an EXPLICITLY reference-compiled dependency.
#[allow(dead_code)]
pub fn expect_box_run_against_ref(tag: &str, lib_src: &str, main: &str) -> Option<String> {
    let libout = compile_lib_ref(tag, lib_src)?;
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    Some(expect_box_run(
        main,
        "Main",
        &[libout, stdlib],
        Some(jdk.as_path()),
    ))
}

/// [`expect_box_run_against_with_reflect`] with an EXPLICITLY reference-compiled dependency.
#[allow(dead_code)]
pub fn expect_box_run_against_with_reflect_ref(
    tag: &str,
    lib_src: &str,
    main: &str,
) -> Option<String> {
    let libout = compile_lib_ref(tag, lib_src)?;
    let stdlib = stdlib_jar();
    let reflect =
        dist_jar("kotlin-reflect.jar").or_else(|| find_jar("kotlin-reflect-", &["sources"]))?;
    let jdk = jdk_modules();
    Some(expect_box_run(
        main,
        "Main",
        &[libout, stdlib, reflect],
        Some(jdk.as_path()),
    ))
}

/// [`expect_box_ok_against`] with an EXPLICITLY reference-compiled dependency.
#[allow(dead_code)]
pub fn expect_box_ok_against_ref(tag: &str, lib_src: &str, main: &str) {
    let libout = compile_lib_ref(tag, lib_src).expect("reference compiler unavailable");
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    let classpath = [libout, stdlib];
    let output =
        compile_and_run_box(main, "Main", &classpath, Some(jdk.as_path())).unwrap_or_else(|| {
            let diagnostics = front_end_diagnostics(main, &classpath, Some(jdk.as_path()));
            panic!("{tag}: compile/run returned None; diagnostics: {diagnostics:?}")
        });
    assert_eq!(output, "OK", "{tag}");
}

/// [`diagnostics_against`] with an EXPLICITLY reference-compiled dependency.
#[allow(dead_code)]
pub fn diagnostics_against_ref(tag: &str, lib_src: &str, main: &str) -> Option<Vec<String>> {
    let libout = compile_lib_ref(tag, lib_src)?;
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    Some(front_end_diagnostics(
        main,
        &[libout, stdlib],
        Some(jdk.as_path()),
    ))
}

/// [`checker_diags_against`] with an EXPLICITLY reference-compiled dependency.
#[allow(dead_code)]
pub fn checker_diags_against_ref(tag: &str, lib_src: &str, main: &str) -> Option<Vec<String>> {
    let libout = compile_lib_ref(tag, lib_src)?;
    let stdlib = stdlib_jar();
    let mut classpath = vec![libout, stdlib];
    classpath.push(jdk_modules());
    Some(inspect_checker_with_classpath(main, classpath, |_, _, _| ()).0)
}

/// Compile a dependency source set with the REFERENCE kotlinc (pooled server) into a scratch
/// classpath dir. `None` = toolchain unavailable; kotlinc REJECTING the sources panics — the
/// fixture is invalid Kotlin, which must never read as a skip.
#[allow(dead_code)]
fn kotlinc_lib_out(sources: &[(&str, &str)]) -> Option<PathBuf> {
    let stdlib = stdlib_jar();
    let work = scratch_dir()?;
    let out = work.join("libout");
    std::fs::create_dir_all(&out).ok()?;
    let mut args = vec![
        "-d".into(),
        out.to_string_lossy().into_owned(),
        "-cp".into(),
        stdlib.to_string_lossy().into_owned(),
    ];
    for (name, src) in sources {
        let path = work.join(name);
        std::fs::write(&path, src).ok()?;
        args.push(path.to_string_lossy().into_owned());
    }
    match kotlinc_compile(&args) {
        Some((0, _)) => Some(out),
        Some((code, err)) => panic!("kotlinc(lib) failed ({code}): {err}"),
        None => None,
    }
}

/// Builder-style fixture for JVM-backed e2e tests that DECLARES its environment requirements and
/// FAILS LOUDLY — with the exact missing piece and how to provision it — where the older `Option`
/// helpers silently return `None` and let a misconfigured environment report as "passed".
///
/// ```ignore
/// common::Fixture::new()
///     .lib("Lib.kt", "package lib\nfun x() = 1\n")
///     .assert_box_ok("import lib.x\nfun box() = if (x() == 1) \"OK\" else \"fail\"\n");
/// ```
///
/// Prefer this for new tests; the `run_box_against`/`expect_box_ok_against` family stays for the
/// existing callers until they migrate.
#[allow(dead_code)]
#[derive(Default)]
pub struct Fixture {
    libs: Vec<(String, String)>,
    ref_libs: Vec<(String, String)>,
    reflect: bool,
}

#[allow(dead_code)]
impl Fixture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a dependency source onto the classpath — krusty-built with kotlinc fallback/cross-check
    /// (see [`compile_libs`]).
    pub fn lib(mut self, file_name: &str, src: &str) -> Self {
        self.libs.push((file_name.to_string(), src.to_string()));
        self
    }

    /// Add a dependency source EXPLICITLY compiled by the reference kotlinc — for tests whose
    /// contract is consuming kotlinc-emitted metadata shapes krusty doesn't produce (see
    /// [`compile_lib_ref`]).
    pub fn reference_lib(mut self, file_name: &str, src: &str) -> Self {
        self.ref_libs.push((file_name.to_string(), src.to_string()));
        self
    }

    /// Put `kotlin-reflect` on the classpath too.
    pub fn with_reflect(mut self) -> Self {
        self.reflect = true;
        self
    }

    /// The dependency build plus the non-lib classpath tail, panicking with the precise
    /// misconfiguration — never skipping — when a required piece is absent.
    fn build_and_extra_cp(&self) -> (Option<Arc<LibBuild>>, Vec<PathBuf>) {
        let build = if self.libs.is_empty() {
            None
        } else {
            let sources: Vec<(&str, &str)> = self
                .libs
                .iter()
                .map(|(n, s)| (n.as_str(), s.as_str()))
                .collect();
            Some(compile_libs_build("fixture", &sources).unwrap_or_else(|| {
                panic!("scratch filesystem unavailable while building this test's dependency lib")
            }))
        };
        let mut extra = Vec::new();
        if !self.ref_libs.is_empty() {
            let sources: Vec<(&str, &str)> = self
                .ref_libs
                .iter()
                .map(|(n, s)| (n.as_str(), s.as_str()))
                .collect();
            let refout = compile_libs_ref("fixture", &sources).unwrap_or_else(|| {
                panic!(
                    "this test's dependency lib is declared reference-compiled and no kotlinc \
                     dist is provisioned.\n\
                     Run `just kotlinc \"$(just max-version)\"` (or `./run-tests.sh`, which \
                     self-provisions) — do NOT let this test pass without it."
                )
            });
            extra.push(refout);
        }
        extra.push(stdlib_jar()); // panics with provisioning instructions when absent
        if self.reflect {
            let reflect = dist_jar("kotlin-reflect.jar")
                .or_else(|| find_jar("kotlin-reflect-", &["sources"]))
                .unwrap_or_else(|| {
                    panic!(
                        "this test needs kotlin-reflect.jar and the provisioned kotlinc dist \
                         doesn't carry one.\n\
                         Run `just kotlinc \"$(just max-version)\"` to fetch the full dist."
                    )
                });
            extra.push(reflect);
        }
        (build, extra)
    }

    /// Compile `main` in-process and run its `box()` on the pooled runner JVM. A front-end
    /// rejection panics with its diagnostics; a missing toolchain panics with the provisioning
    /// command; there is no silent-skip path. Dependency libs follow the krusty-first /
    /// kotlinc-fallback / optional-cross-check contract of [`compile_libs`].
    pub fn run_box(&self, main: &str) -> String {
        let (build, extra) = self.build_and_extra_cp();
        let jdk = jdk_modules(); // panics with JAVA_HOME diagnosis when absent
        match build {
            None => expect_box_run(main, "Main", &extra, Some(jdk.as_path())),
            Some(build) => {
                box_against_build("fixture", &build, main, &extra, &jdk).unwrap_or_else(|| {
                    let mut cp = vec![build.krusty_out().to_path_buf()];
                    cp.extend_from_slice(&extra);
                    let diagnostics = front_end_diagnostics(main, &cp, Some(jdk.as_path()));
                    panic!("fixture: compile/run returned None; diagnostics: {diagnostics:?}")
                })
            }
        }
    }

    /// [`Fixture::run_box`] asserting the canonical `"OK"`.
    pub fn assert_box_ok(&self, main: &str) {
        let out = self.run_box(main);
        assert_eq!(out, "OK");
    }
}

/// Compile `main` against a `lib_src` dependency and run its `box()` on the persistent JVM. The lib
/// is krusty-built when krusty can build AND `main` can consume it, with the reference kotlinc as a
/// lazy fallback for either gap (see [`compile_libs`]); the default-on cross-check additionally runs
/// `main` against the kotlinc-built lib and asserts the same result. `None` (→ skip) when the
/// toolchain is unavailable. stdlib + JDK modules are on both the compile and run classpath.
#[allow(dead_code)]
pub fn run_box_against(tag: &str, lib_src: &str, main: &str) -> Option<String> {
    let build = compile_libs_build(tag, &[("Lib.kt", lib_src)])?;
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    box_against_build(tag, &build, main, &[stdlib], &jdk)
}

/// The shared consumption path of every `*_against` box helper: `main` compiles and runs against
/// the krusty-built lib — no reference fallback; a gap surfaces at the caller as a descriptive
/// failure. Under the default-on cross-check the result is also compared against the
/// kotlinc-built lib (opt out: `KRUSTY_LIB_CROSSCHECK=0`).
#[allow(dead_code)]
fn box_against_build(
    tag: &str,
    build: &LibBuild,
    main: &str,
    extra_cp: &[PathBuf],
    jdk: &Path,
) -> Option<String> {
    let mut cp = vec![build.krusty_out().to_path_buf()];
    cp.extend_from_slice(extra_cp);
    let result = compile_and_run_box(main, "Main", &cp, Some(jdk))?;
    build.cross_check_box(tag, main, extra_cp, &result);
    Some(result)
}

/// [`run_box_against`] with the strict contract: `None` means ONLY that the kotlinc/JVM toolchain
/// isn't provisioned. A `main` the front end REJECTS panics with its diagnostics instead of
/// collapsing into the same `None` and reporting as a passing skip.
#[allow(dead_code)]
pub fn expect_box_run_against(tag: &str, lib_src: &str, main: &str) -> Option<String> {
    let build = compile_libs_build(tag, &[("Lib.kt", lib_src)])?;
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    match box_against_build(tag, &build, main, std::slice::from_ref(&stdlib), &jdk) {
        Some(result) => Some(result),
        None => {
            // main couldn't consume the krusty-built lib (or the run failed) — surface the
            // diagnostics, never a silent skip.
            let cp = [build.krusty_out().to_path_buf(), stdlib];
            let diagnostics = front_end_diagnostics(main, &cp, Some(jdk.as_path()));
            panic!("{tag}: compile/run returned None; diagnostics: {diagnostics:?}")
        }
    }
}

/// [`expect_box_run_against`] with `kotlin-reflect` on the classpath.
#[allow(dead_code)]
pub fn expect_box_run_against_with_reflect(tag: &str, lib_src: &str, main: &str) -> Option<String> {
    let build = compile_libs_build(tag, &[("Lib.kt", lib_src)])?;
    let stdlib = stdlib_jar();
    let reflect =
        dist_jar("kotlin-reflect.jar").or_else(|| find_jar("kotlin-reflect-", &["sources"]))?;
    let jdk = jdk_modules();
    match box_against_build(tag, &build, main, &[stdlib.clone(), reflect.clone()], &jdk) {
        Some(result) => Some(result),
        None => {
            let cp = [build.krusty_out().to_path_buf(), stdlib, reflect];
            let diagnostics = front_end_diagnostics(main, &cp, Some(jdk.as_path()));
            panic!("{tag}: compile/run returned None; diagnostics: {diagnostics:?}")
        }
    }
}

/// Frontend diagnostics for `main` against a source dependency (krusty-built when possible, else
/// reference-built — see [`compile_libs`]).
#[allow(dead_code)]
pub fn diagnostics_against(tag: &str, lib_src: &str, main: &str) -> Option<Vec<String>> {
    let libout = compile_lib(tag, lib_src)?;
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    Some(front_end_diagnostics(
        main,
        &[libout, stdlib],
        Some(jdk.as_path()),
    ))
}

/// Run a classpath fixture, skipping only when the external toolchain is unavailable.
#[allow(dead_code)]
pub fn expect_box_ok_against(tag: &str, lib_src: &str, main: &str) {
    let Some(build) = compile_libs_build(tag, &[("Lib.kt", lib_src)]) else {
        return;
    };
    let stdlib = stdlib_jar();
    let jdk = jdk_modules();
    let output = box_against_build(tag, &build, main, std::slice::from_ref(&stdlib), &jdk)
        .unwrap_or_else(|| {
            let cp = [build.krusty_out().to_path_buf(), stdlib.clone()];
            let diagnostics = front_end_diagnostics(main, &cp, Some(jdk.as_path()));
            let backend = diagnostics
                .is_empty()
                .then(|| backend_outcome_in_process(main, "Main", &cp, Some(jdk.as_path())));
            panic!(
                "{tag}: compile/run returned None; diagnostics: {diagnostics:?}; backend: {backend:?}"
            )
        });
    assert_eq!(output, "OK", "{tag}");
}

/// Compile `main` against a kotlinc-built `lib_src` up to the CHECKER only (no lowering/emit), returning
/// the diagnostic messages (empty = clean). For asserting the RESOLUTION of a shape whose end-to-end
/// lowering is an orthogonal, not-yet-implemented feature. `None` (→ skip) when the toolchain is absent.
#[allow(dead_code)]
pub fn checker_diags_against(tag: &str, lib_src: &str, main: &str) -> Option<Vec<String>> {
    let libout = compile_lib(tag, lib_src)?;
    let stdlib = stdlib_jar();
    let mut classpath = vec![libout, stdlib];
    classpath.push(jdk_modules());
    Some(inspect_checker_with_classpath(main, classpath, |_, _, _| ()).0)
}

/// Check `main` against the Kotlin stdlib without lowering or emitting.
#[allow(dead_code)]
pub fn checker_diags_with_stdlib(main: &str) -> Option<Vec<String>> {
    let stdlib = stdlib_jar();
    let mut classpath = vec![stdlib];
    classpath.push(jdk_modules());
    Some(inspect_checker_with_classpath(main, classpath, |_, _, _| ()).0)
}

pub fn inspect_checker_with_classpath<T>(
    main: &str,
    classpath: Vec<PathBuf>,
    inspect: impl FnOnce(
        &krusty::ast::File,
        &krusty::frontend::FrontendTypeInfo,
        &krusty::frontend::FrontendSymbols,
    ) -> T,
) -> (Vec<String>, T) {
    use krusty::diag::DiagSink;
    use krusty::frontend::{check_file, collect_signatures_with_cp};
    let mut diags = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(main);
    let toks = krusty::lexer::lex(main, &mut diags);
    let files = vec![krusty::parser::parse_with_features(
        main, &toks, &mut diags, &features,
    )];
    let cp = std::rc::Rc::new(Classpath::new(classpath));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    let info = check_file(&files[0], &mut syms, &mut diags);
    let inspected = inspect(&files[0], &info, &syms);
    (
        diags.diags.iter().map(|m| m.msg.clone()).collect(),
        inspected,
    )
}

/// Whether both the JVM toolchain AND the box corpus are provisioned (an e2e that runs a corpus case
/// needs both). `false` ⇒ the test should skip.
#[allow(dead_code)]
pub fn corpus_ready() -> bool {
    // Library forms again — a probe must report, not panic. JAVA_HOME is checked SEPARATELY from the
    // jimage: `KRUSTY_SURVEY_JDK_MODULES` can supply the latter while the former is unset, and the box
    // runner needs a `java` binary, so reporting "ready" on the jimage alone would panic downstream.
    std::env::var("KRUSTY_REF_JAVA_HOME")
        .or_else(|_| std::env::var("JAVA_HOME"))
        .is_ok_and(|home| !home.is_empty())
        && krusty::toolchain::stdlib_jar().is_some()
        && krusty::toolchain::jdk_modules().is_some()
        && box_corpus_dir().is_some()
}

/// Compile + run a SINGLE box-corpus case by its path relative to the corpus root (e.g.
/// `"boxing/boxing10.kt"`), reusing the EXACT classpath the conformance gate compiles with —
/// `classpath_jars_for` (the canonical, directive-aware set: stdlib/test/annotations unconditional,
/// +reflect/coroutines per directive), so the e2e and the gate can't drift. That set always carries
/// the stdlib, so emitted `Intrinsics` null-checks resolve at runtime too (it serves as both the
/// compile and the runtime classpath); the JDK `lib/modules` jimage is the compile bootclasspath.
///
/// Returns `Some(box_result)` when the case actually RAN ("OK" when correct), or `None` when it was
/// SKIPPED — corpus/toolchain absent, a multi-file `// FILE:`/`// MODULE:` case (this single-source
/// helper doesn't split them; the full gate does), or a case krusty declines to compile. Callers must
/// treat `None` as a skip (matching the gate's skip accounting), NOT a failure.
#[allow(dead_code)]
pub fn run_box_corpus_case(rel: &str) -> Option<String> {
    let src = krusty::conformance::prepare_test_source(
        &std::fs::read_to_string(box_corpus_dir()?.join(rel)).ok()?,
    );
    // Multi-file / multi-module cases need the gate's `// FILE:`/`// MODULE:` splitting — skip here
    // rather than miscompile all blocks as one source (enforce the contract, don't rely on luck).
    if src.contains("// FILE:") || src.contains("// MODULE:") {
        return None;
    }
    let jdk = jdk_modules();
    let cp = classpath_jars_for(&src);
    let classes = compile_in_process(&src, "P", &cp, Some(jdk.as_path()))?;
    let box_class = find_box_class(&classes)?;
    run_box(&classes, &box_class, &cp)
}

#[allow(dead_code)]
pub fn box_corpus_case_backend_outcome(rel: &str) -> Option<BackendOutcome> {
    let src = krusty::conformance::prepare_test_source(
        &std::fs::read_to_string(box_corpus_dir()?.join(rel)).ok()?,
    );
    if src.contains("// FILE:") || src.contains("// MODULE:") {
        return None;
    }
    let jdk = jdk_modules();
    let cp = classpath_jars_for(&src);
    backend_outcome_in_process(&src, "P", &cp, Some(jdk.as_path()))
}

// --- Persistent kotlinc compiler server -----------------------------------
//
// The reference `kotlinc` is a JVM program; spawning its CLI per test pays a ~2-4s JVM + compiler cold
// start each time (the dominant cost of the differential e2e). Reusing `K2JVMCompiler.exec()` in one
// persistent JVM is warm (~0.4s) BUT leaks: the compiler accumulates global caches (its IntelliJ-core
// application environment + jar-filesystem handlers) across calls, so the 2nd+ compile in one process
// death-spirals the collector (1st ~4s, 2nd >120s, independent of heap). The official compile daemon
// avoids this not by magic but by CLEARING those caches between compiles.
//
// So this driver does the same, in ONE JVM: it holds a single `URLClassLoader` over the compiler jars
// (classes loaded ONCE — that is where the warmth is), runs each request through `K2JVMCompiler.exec()`,
// then resets the leaky global via `KotlinCoreEnvironment.disposeApplicationEnvironment()`. The next
// compile recreates a fresh application environment, so state never accumulates. Result: ~0.4s warm
// compiles, STABLE across compiles (measured 3990/417/523/422/400ms), in a single ~1 GB JVM — no second
// daemon process (fits small/shared RAM), no RMI, no per-compile class reload. The driver uses only JDK
// APIs (reflection + URLClassLoader), so it needs no compiler jar on its OWN classpath; it builds the
// loader from the dist lib dir passed as argv[0].
const KOTLINC_SERVER_SRC: &str = r#"
import java.io.*;
import java.net.*;
import java.util.*;
import java.lang.reflect.Method;

public class KotlincServer {
    public static void main(String[] a) throws Exception {
        // Compiler jars for the loader — drop `-sources`/JS/WASM jars (not needed to compile plain JVM
        // Kotlin) so the one loader holds less.
        File[] files = new File(a[0]).listFiles();
        ArrayList<URL> urls = new ArrayList<>();
        if (files != null) for (File f : files) {
            String n = f.getName();
            if (n.endsWith(".jar") && !n.endsWith("-sources.jar") && !n.contains("-js") && !n.contains("-wasm"))
                urls.add(f.toURI().toURL());
        }
        // ONE loader for the whole session: the compiler classes load once (this is the warmth). Parent is
        // the platform loader only, so the compiler's classes stay private to it.
        URLClassLoader cl = new URLClassLoader(urls.toArray(new URL[0]), ClassLoader.getPlatformClassLoader());
        Class<?> k = cl.loadClass("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler");
        Method exec = k.getMethod("exec", PrintStream.class, String[].class);
        // The reset the compile daemon uses: dispose the accumulated global application environment after
        // each compile so the next one starts clean — without this the reused compiler leaks and stalls.
        Method disposeAppEnv = cl.loadClass("org.jetbrains.kotlin.cli.jvm.compiler.KotlinCoreEnvironment")
            .getMethod("disposeApplicationEnvironment");

        // Bind the framed protocol to the RAW stdin/stdout fds, THEN redirect System.out to stderr, so the
        // compiler's own prints to System.out cannot corrupt a response frame.
        DataInputStream din = new DataInputStream(new BufferedInputStream(new FileInputStream(FileDescriptor.in), 65536));
        DataOutputStream dout = new DataOutputStream(new BufferedOutputStream(new FileOutputStream(FileDescriptor.out), 4096));
        System.setOut(System.err);

        while (true) {
            int n;
            try { n = din.readInt(); } catch (EOFException e) { break; }
            String[] args = new String[n];
            for (int i = 0; i < n; i++) {
                int l = din.readUnsignedShort();
                args[i] = new String(din.readNBytes(l), "UTF-8");
            }
            ByteArrayOutputStream errBuf = new ByteArrayOutputStream();
            PrintStream err = new PrintStream(errBuf, true, "UTF-8");
            int codeNum;
            try {
                Object comp = k.getDeclaredConstructor().newInstance();
                Object code = exec.invoke(comp, err, (Object) args);
                codeNum = (int) code.getClass().getMethod("getCode").invoke(code);
            } catch (Throwable t) {
                t.printStackTrace(err);
                codeNum = 2;
            } finally {
                // Clear the leaky global compiler state — the key to reusing one JVM without degrading.
                try { disposeAppEnv.invoke(null); } catch (Throwable ignore) {}
            }
            byte[] eb = errBuf.toByteArray();
            dout.writeInt(codeNum);
            dout.writeInt(eb.length);
            dout.write(eb);
            dout.flush();
        }
    }
}
"#;

/// The reference compiler's all-in-one jar (`<dist>/lib/kotlin-compiler.jar`), which carries
/// `K2JVMCompiler`. `None` when the provisioned dist is unavailable.
#[allow(dead_code)]
pub fn kotlin_compiler_jar() -> Option<PathBuf> {
    let p = kotlinc_lib_dir()?.join("kotlin-compiler.jar");
    p.is_file().then_some(p)
}

/// Compile the pure-JDK `KotlincServer.java` driver once (via `javac`) into a stable cache dir; return it.
/// The driver uses only reflection + `URLClassLoader`, so it needs no compiler jar to compile OR to run.
fn setup_kotlinc_server(java_home: &str, _compiler_jar: &Path) -> Option<PathBuf> {
    // The JDK is part of the cache key: a class compiled by a NEWER javac (another session's
    // JAVA_HOME) is unloadable by an older runtime, and the server-spawn failure then reads as
    // "kotlinc unavailable" — silently disabling every reference compile and cross-check.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in KOTLINC_SERVER_SRC.bytes().chain(java_home.bytes()) {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/kotlinc_server_{hash:016x}"));
    if dir.join("KotlincServer.class").is_file() {
        return Some(dir);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let src_path = dir.join("KotlincServer.java");
    std::fs::write(&src_path, KOTLINC_SERVER_SRC).ok()?;
    let javac = format!("{java_home}/bin/javac");
    if !Path::new(&javac).exists() {
        return None;
    }
    let out = Command::new(&javac)
        .arg("-d")
        .arg(&dir)
        .arg(&src_path)
        .output()
        .ok()?;
    if !dir.join("KotlincServer.class").is_file() {
        eprintln!(
            "KotlincServer javac failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return None;
    }
    Some(dir)
}

/// The reference compiler dist's lib dir (holds `kotlin-compiler.jar`) — the jars the isolating-loader
/// driver builds its per-compile `URLClassLoader` from. Passed to the driver as its argv[0].
#[allow(dead_code)]
fn kotlinc_lib_of(compiler_jar: &Path) -> Option<PathBuf> {
    compiler_jar.parent().map(Path::to_path_buf)
}

/// A persistent JVM running the in-process `KotlincServer` compiler, fed compiler arg-lists over a pipe.
struct KotlincServer {
    _child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl KotlincServer {
    /// `cp` is the driver's run classpath (just its `server_dir` — the driver is pure JDK and loads the
    /// compiler itself); `lib_dir` is passed as argv[0] so the driver builds its compiler `URLClassLoader`.
    fn new(java: &str, cp: &str, lib_dir: &str) -> Option<Self> {
        let mut cmd = Command::new(java);
        // One persistent JVM that compiles in-process. 1 GB holds a single compile's working set (the leaky
        // global state is reset after each — see the driver), so it stays flat across compiles. Fast-startup
        // JIT/GC since each compile is short.
        cmd.args(jvm_gclog_args("kotlinc"));
        cmd.args([
            "-XX:TieredStopAtLevel=1",
            "-XX:+UseSerialGC",
            "-Xmx1g",
            "-cp",
            cp,
            "KotlincServer",
            lib_dir,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        die_with_parent(&mut cmd);
        let mut child = spawn_owned(cmd).ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        Some(KotlincServer {
            _child: child,
            stdin,
            stdout,
        })
    }

    fn try_compile(&mut self, args: &[String]) -> std::io::Result<(i32, String)> {
        self.stdin.write_all(&(args.len() as u32).to_be_bytes())?;
        for arg in args {
            self.stdin.write_all(&(arg.len() as u16).to_be_bytes())?;
            self.stdin.write_all(arg.as_bytes())?;
        }
        self.stdin.flush()?;
        // A compile can take a few seconds (cold) — generous deadline.
        let deadline = Instant::now() + Duration::from_secs(120);
        let fd = self.stdout.as_raw_fd();
        let mut i32_buf = [0u8; 4];
        read_exact_deadline(fd, &mut i32_buf, deadline)?;
        let code = i32::from_be_bytes(i32_buf);
        read_exact_deadline(fd, &mut i32_buf, deadline)?;
        let elen = u32::from_be_bytes(i32_buf) as usize;
        let mut err = vec![0u8; elen];
        read_exact_deadline(fd, &mut err, deadline)?;
        Ok((code, String::from_utf8_lossy(&err).into_owned()))
    }
}

/// Compile with the reference compiler via the persistent server. `args` are ordinary `kotlinc` CLI
/// arguments (`["-d", out, "-cp", cp, "Lib.kt"]`). Returns `(exit_code, stderr)` — `exit_code == 0`
/// is success — or `None` if the toolchain/JVM is unavailable (caller skips, exactly like a missing
/// `kotlinc`). One server JVM is shared across all calls (keyed by the compiler jar).
#[allow(dead_code)]
/// A per-classpath pool of persistent compiler servers, each behind its own `Arc<Mutex>` so callers
/// hold the (brief) map lock only to pick/grow a server, then release it before the long compile.
type ServerPool<S> = Mutex<HashMap<String, Vec<Arc<Mutex<S>>>>>;

pub fn kotlinc_compile(args: &[String]) -> Option<(i32, String)> {
    static POOL: OnceLock<ServerPool<KotlincServer>> = OnceLock::new();
    let _pg = ProfGuard::new("kotlinc");
    let java_home = java_home();
    let java = format!("{java_home}/bin/java");
    if !Path::new(&java).exists() {
        return None;
    }
    let compiler_jar = kotlin_compiler_jar()?;
    let server_dir = setup_kotlinc_server(&java_home, &compiler_jar)?;
    let lib_dir = kotlinc_lib_of(&compiler_jar)?
        .to_string_lossy()
        .into_owned();
    // The driver is pure JDK and loads the compiler itself (from `lib_dir`), so its OWN classpath is just
    // its `server_dir`.
    let cp = server_dir.to_string_lossy().into_owned();
    let server = {
        let pool = POOL.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = pool.lock().unwrap();
        let servers = map.entry(cp.clone()).or_default();
        if let Some(idle) = servers.iter().find(|s| s.try_lock().is_ok()) {
            idle.clone()
        } else if servers.len() < server_pool_cap() {
            let s = Arc::new(Mutex::new(KotlincServer::new(&java, &cp, &lib_dir)?));
            servers.push(s.clone());
            s
        } else {
            // At cap and all busy — block on the least-recently-added (spreads simple contention).
            servers[0].clone()
        }
    };
    let mut server = server.lock().unwrap();
    match server.try_compile(args) {
        Ok(r) => Some(r),
        Err(_) => {
            // Server JVM died — restart once and retry.
            *server = KotlincServer::new(&java, &cp, &lib_dir)?;
            server.try_compile(args).ok()
        }
    }
}

/// How many persistent compiler-server JVMs to pool per classpath. Scales with the host — a single
/// server serializes every kotlinc-dependency compile AND every java-driver test behind one mutex,
/// which measured as the e2e suite's dominant wall-clock cost (hundreds of ~0.4s warm compiles all
/// queueing on one JVM while the other N-1 cores idle). Each kotlinc server is capped at `-Xmx1g`
/// and each JavaRunner at `-Xmx512m`, so the `ncpu/2` default clamped to [1, 6] bounds worst-case
/// footprint at ~6 GB on big hosts and 2 servers on a 4-core CI runner. `KRUSTY_SERVER_POOL`
/// overrides in either direction (e.g. 1 on a swapping shared box).
fn server_pool_cap() -> usize {
    std::env::var("KRUSTY_SERVER_POOL")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or_else(|| {
            let ncpu = std::thread::available_parallelism().map_or(1, |n| n.get());
            (ncpu / 2).clamp(1, 6)
        })
}

// --- Persistent javac+run server ------------------------------------------
//
// Tests that exercise a hand-written Java driver (e.g. invoking a krusty-compiled `suspend` function
// with a `Continuation` from Java) compile `M.java` with `javac` then run it with `java` — two process
// spawns per test, the dominant cost of those suites. Instead run ONE persistent JVM that compiles the
// driver IN-PROCESS (`ToolProvider.getSystemJavaCompiler().run`, no spawn) and runs its `main` via a
// `URLClassLoader` (capturing stdout), all file-path based — the test still writes the sources/classes
// to disk exactly as before. Launched with `-Xverify:all` so loaded classes are verified like the
// per-test `java -Xverify:all` they replace.

const JAVA_RUNNER_SRC: &str = r#"
import javax.tools.*;
import java.io.*;
import java.net.*;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;

public class JavaRunner {
    public static void main(String[] a) throws Exception {
        DataInputStream din = new DataInputStream(new BufferedInputStream(System.in, 65536));
        DataOutputStream dout = new DataOutputStream(new BufferedOutputStream(System.out, 4096));
        PrintStream realOut = System.out;
        while (true) {
            String driver, cp, outdir, mainClass, procPath;
            try { driver = readStr(din); } catch (EOFException e) { break; }
            cp = readStr(din); outdir = readStr(din); mainClass = readStr(din);
            procPath = readStr(din);
            String result;
            try {
                if (mainClass.equals("\u0000javap")) {
                    // Pooled disassembly: `driver` carries the javap argv joined by \n. Runs
                    // in-process via the javap ToolProvider — no per-call JVM start.
                    java.util.spi.ToolProvider jp =
                        java.util.spi.ToolProvider.findFirst("javap").orElse(null);
                    if (jp == null) {
                        result = "ERROR:javap:ToolProvider unavailable";
                    } else {
                        java.io.StringWriter so = new java.io.StringWriter();
                        java.io.StringWriter se = new java.io.StringWriter();
                        int rc = jp.run(new PrintWriter(so, true), new PrintWriter(se, true),
                                driver.split("\n"));
                        result = rc == 0 ? so.toString() : "ERROR:javap:" + se + so;
                    }
                    byte[] rb0 = result.getBytes(StandardCharsets.UTF_8);
                    dout.writeInt(rb0.length);
                    dout.write(rb0);
                    dout.flush();
                    continue;
                }
                ByteArrayOutputStream jerr = new ByteArrayOutputStream();
                JavaCompiler jc = ToolProvider.getSystemJavaCompiler();
                // `driver` is one or more `.java` paths joined by the platform path separator.
                String[] srcs = driver.split(File.pathSeparator);
                ArrayList<String> jargs = new ArrayList<>();
                jargs.add("-cp"); jargs.add(cp);
                jargs.add("-d"); jargs.add(outdir);
                if (!procPath.isEmpty()) {
                    // Annotation processing: javac discovers processors via ServiceLoader on the
                    // processor path and runs its own multi-round loop. JDK >= 23 disables
                    // processing unless explicitly requested, hence -proc:full. Generated sources
                    // land in outdir/apt-src (their classes still go to -d).
                    File gen = new File(outdir, "apt-src");
                    gen.mkdirs();
                    jargs.add("-processorpath"); jargs.add(procPath);
                    jargs.add("-proc:full");
                    jargs.add("-s"); jargs.add(gen.getPath());
                }
                for (String s : srcs) jargs.add(s);
                int rc = jc.run(null, null, new PrintStream(jerr, true, "UTF-8"),
                        jargs.toArray(new String[0]));
                if (rc != 0) {
                    result = "ERROR:javac:" + jerr.toString("UTF-8");
                } else if (mainClass.isEmpty()) {
                    // Compile-only mode: the caller just wants the `.class` files in `outdir`.
                    result = "OK";
                } else {
                    // Classpath for running: outdir + the given cp entries.
                    String[] parts = cp.split(File.pathSeparator);
                    URL[] urls = new URL[parts.length + 1];
                    urls[0] = new File(outdir).toURI().toURL();
                    for (int i = 0; i < parts.length; i++) urls[i + 1] = new File(parts[i]).toURI().toURL();
                    ByteArrayOutputStream capture = new ByteArrayOutputStream();
                    PrintStream cps = new PrintStream(capture, true, "UTF-8");
                    System.setOut(cps);
                    try (URLClassLoader ldr = new URLClassLoader(urls, ClassLoader.getSystemClassLoader())) {
                        Class<?> cls = Class.forName(mainClass, true, ldr);
                        cls.getMethod("main", String[].class).invoke(null, (Object) new String[0]);
                        result = capture.toString("UTF-8");
                    } catch (Throwable t) {
                        Throwable c = t.getCause() != null ? t.getCause() : t;
                        result = "ERROR:run:" + c;
                    } finally {
                        System.setOut(realOut);
                    }
                }
            } catch (Throwable t) {
                result = "ERROR:" + t;
            }
            byte[] rb = result.getBytes(StandardCharsets.UTF_8);
            dout.writeInt(rb.length);
            dout.write(rb);
            dout.flush();
        }
    }

    static String readStr(DataInputStream in) throws IOException {
        int n = in.readInt();
        return new String(in.readNBytes(n), StandardCharsets.UTF_8);
    }
}
"#;

fn setup_java_runner(java_home: &str) -> Option<PathBuf> {
    // JDK in the cache key for the same reason as `setup_kotlinc_server`: a newer-javac class
    // under an older runtime dies at load and the failure masquerades as "toolchain unavailable".
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in JAVA_RUNNER_SRC.bytes().chain(java_home.bytes()) {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/java_runner_{hash:016x}"));
    if dir.join("JavaRunner.class").is_file() {
        return Some(dir);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let src_path = dir.join("JavaRunner.java");
    std::fs::write(&src_path, JAVA_RUNNER_SRC).ok()?;
    let javac = format!("{java_home}/bin/javac");
    if !Path::new(&javac).exists() {
        return None;
    }
    let out = Command::new(&javac)
        .args(["-d", &dir.to_string_lossy()])
        .arg(&src_path)
        .output()
        .ok()?;
    out.status.success().then_some(dir)
}

struct JavaRunner {
    _child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl JavaRunner {
    fn new(java: &str, runner_dir: &Path) -> Option<Self> {
        let mut cmd = Command::new(java);
        // Cap heap for footprint, but keep the default collector + full tiered JIT: JavaRunner executes
        // the reference javac output on a thread pool, so serial GC / C1-only throttles it (see BoxRunner).
        cmd.args([
            "-Xverify:all",
            "-Xmx512m",
            "-cp",
            &runner_dir.to_string_lossy(),
            "JavaRunner",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        die_with_parent(&mut cmd);
        let mut child = spawn_owned(cmd).ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        Some(JavaRunner {
            _child: child,
            stdin,
            stdout,
        })
    }

    fn write_str(&mut self, s: &str) -> std::io::Result<()> {
        self.stdin.write_all(&(s.len() as u32).to_be_bytes())?;
        self.stdin.write_all(s.as_bytes())
    }

    fn try_run(
        &mut self,
        driver: &str,
        cp: &str,
        outdir: &str,
        main_class: &str,
        proc_path: &str,
    ) -> std::io::Result<String> {
        self.write_str(driver)?;
        self.write_str(cp)?;
        self.write_str(outdir)?;
        self.write_str(main_class)?;
        self.write_str(proc_path)?;
        self.stdin.flush()?;
        let deadline = Instant::now() + Duration::from_secs(60);
        let fd = self.stdout.as_raw_fd();
        let mut len = [0u8; 4];
        read_exact_deadline(fd, &mut len, deadline)?;
        let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
        read_exact_deadline(fd, &mut buf, deadline)?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

/// Compile a Java `driver_path` against `cp` into `outdir` and run `main_class`'s `main`, in ONE
/// persistent JVM (in-process javac + classloader, no per-call `javac`/`java` spawn). Returns the
/// driver's stdout (or `ERROR:…`), or `None` if the JDK/JVM is unavailable. `cp` is the run+compile
/// classpath (krusty output dirs + stdlib); `outdir` receives the driver's `.class`.
#[allow(dead_code)]
pub fn javac_run(driver_path: &str, cp: &str, outdir: &str, main_class: &str) -> Option<String> {
    javac_run_proc(driver_path, cp, outdir, main_class, "")
}

/// [`javac_run`] with an annotation `-processorpath` (empty = no processing) — javac runs its own
/// multi-round APT loop in the same persistent JVM.
#[allow(dead_code)]
pub fn javac_run_proc(
    driver_path: &str,
    cp: &str,
    outdir: &str,
    main_class: &str,
    proc_path: &str,
) -> Option<String> {
    // A POOL of runner JVMs (not one global), so Java-driver tests run N-wide instead of serializing
    // on a single mutex held across the whole javac+run. The pool lock is released before the run.
    static POOL: OnceLock<Mutex<Vec<Arc<Mutex<JavaRunner>>>>> = OnceLock::new();
    let java_home = java_home();
    let java = format!("{java_home}/bin/java");
    if !Path::new(&java).exists() {
        return None;
    }
    let runner_dir = setup_java_runner(&java_home)?;
    let runner = {
        let pool = POOL.get_or_init(|| Mutex::new(Vec::new()));
        let mut v = pool.lock().unwrap();
        if let Some(idle) = v.iter().find(|r| r.try_lock().is_ok()) {
            idle.clone()
        } else if v.len() < server_pool_cap() {
            let r = Arc::new(Mutex::new(JavaRunner::new(&java, &runner_dir)?));
            v.push(r.clone());
            r
        } else {
            v[0].clone()
        }
    };
    let mut runner = runner.lock().unwrap();
    match runner.try_run(driver_path, cp, outdir, main_class, proc_path) {
        Ok(s) => Some(s),
        Err(_) => {
            *runner = JavaRunner::new(&java, &runner_dir)?;
            runner
                .try_run(driver_path, cp, outdir, main_class, proc_path)
                .ok()
        }
    }
}

/// Disassemble via the pooled JavaRunner's in-process `javap` ToolProvider — the same persistent
/// JVM the driver tests use, so a parity test costs no `javap` process (a full JVM start) per
/// assertion. `args` is the ordinary javap argv (e.g. `["-c", "-p", "/path/To.class"]`). Returns
/// the disassembly text; panics on a javap error (a malformed class under test is a failure, not a
/// skip); `None` = JVM unavailable.
#[allow(dead_code)]
pub fn javap(args: &[&str]) -> Option<String> {
    let joined = args.join("\n");
    let out = javac_run_proc(&joined, "", "", "\u{0}javap", "")?;
    if let Some(err) = out.strip_prefix("ERROR:javap:") {
        panic!("pooled javap failed: {err}");
    }
    Some(out)
}

/// A javac compile's output: the class directory (a valid krusty classpath entry: loose `.class`
/// files) plus every emitted class as `(binary-name-with-slashes, bytes)`.
pub type JavacOutput = (PathBuf, Vec<(String, Vec<u8>)>);

/// The whole Java-interop e2e idiom in one strict call: javac-compile `java_sources` (pooled
/// in-process javac), krusty-compile Kotlin `use_src` (file stem `Use`, declaring `fun box()`)
/// against that classpath in-process, then run a Java driver invoking `UseKt.box()` in the pooled
/// JavaRunner and return its trimmed stdout. Every step panics with the failing stage — a
/// misconfigured environment or a krusty gap fails the test, it never skips.
#[allow(dead_code)]
pub fn java_interop_box(tag: &str, java_sources: &[(&str, &str)], use_src: &str) -> String {
    let jdk = jdk_modules(); // panics with JAVA_HOME diagnosis when absent
    let stdlib = stdlib_jar();
    let sources: Vec<(String, String)> = java_sources
        .iter()
        .map(|(n, s)| ((*n).to_string(), (*s).to_string()))
        .collect();
    let (cp, _) = javac_compile(&sources, &[])
        .unwrap_or_else(|| panic!("{tag}: pooled javac failed on the Java fixture"));
    let kr = scratch_dir().unwrap_or_else(|| panic!("{tag}: scratch filesystem unavailable"));
    compile_to_dir(
        use_src,
        "Use",
        std::slice::from_ref(&cp),
        Some(jdk.as_path()),
        &kr,
    )
    .unwrap_or_else(|| {
        let diagnostics =
            front_end_diagnostics(use_src, std::slice::from_ref(&cp), Some(jdk.as_path()));
        panic!("{tag}: krusty failed against the Java fixture; diagnostics: {diagnostics:?}")
    });
    let main =
        "public class M { public static void main(String[] a) { System.out.println(UseKt.box()); } }";
    let m_path = kr.join("M.java");
    std::fs::write(&m_path, main).unwrap_or_else(|e| panic!("{tag}: write driver: {e}"));
    let kcp = format!(
        "{}:{}:{}",
        kr.to_string_lossy(),
        cp.to_string_lossy(),
        stdlib.display()
    );
    let out = javac_run(&m_path.to_string_lossy(), &kcp, &kr.to_string_lossy(), "M")
        .unwrap_or_else(|| panic!("{tag}: pooled JavaRunner unavailable"));
    out.trim().to_string()
}

/// Compile a set of Java sources `(file_name, source)` against `cp` with the persistent JavaRunner's
/// in-process javac (no `javac` process spawn — the same JVM the Java-driver e2e suites reuse).
/// The classes come back both as a classpath dir and as bytes so callers can also hand them to
/// BoxRunner's in-memory classloader. `None` if the JDK is unavailable or javac rejects the sources
/// — for the conformance harness that is a SKIP, not a failure.
#[allow(dead_code)]
pub fn javac_compile(sources: &[(String, String)], cp_jars: &[PathBuf]) -> Option<JavacOutput> {
    javac_compile_proc(sources, cp_jars, &[])
}

/// [`javac_compile`] with annotation processors: `proc_path` entries (dirs or jars, each carrying
/// `META-INF/services/javax.annotation.processing.Processor`) go on javac's `-processorpath`, and
/// javac runs its own multi-round APT loop in-process — generated sources are compiled in the same
/// invocation and their classes come back with the rest.
#[allow(dead_code)]
pub fn javac_compile_proc(
    sources: &[(String, String)],
    cp_jars: &[PathBuf],
    proc_path: &[PathBuf],
) -> Option<JavacOutput> {
    if sources.is_empty() {
        return None;
    }
    let root = scratch_dir()?;
    let srcdir = root.join("src");
    let outdir = root.join("classes");
    std::fs::create_dir_all(&srcdir).ok()?;
    std::fs::create_dir_all(&outdir).ok()?;
    let mut paths: Vec<String> = Vec::new();
    for (name, text) in sources {
        // Leaf name only — javac requires the file name to match the public class it declares.
        let leaf = name.rsplit('/').next().unwrap_or(name);
        let p = srcdir.join(leaf);
        std::fs::write(&p, text).ok()?;
        paths.push(p.to_string_lossy().into_owned());
    }
    // Must match `File.pathSeparator` in the JavaRunner JVM — the protocol splits on it.
    let sep = if cfg!(windows) { ";" } else { ":" };
    let joined = paths.join(sep);
    let cp = if cp_jars.is_empty() {
        ".".to_string()
    } else {
        cp_jars
            .iter()
            .map(|j| j.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(sep)
    };
    let proc = proc_path
        .iter()
        .map(|j| j.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(":");
    // Empty main class = compile-only (no run) in the JavaRunner protocol.
    let res = javac_run_proc(&joined, &cp, &outdir.to_string_lossy(), "", &proc)?;
    if res != "OK" {
        // Failure is a legitimate SKIP for the harness; surface the javac error only on demand.
        if std::env::var("KRUSTY_JAVAC_DEBUG").is_ok() {
            eprintln!("[javac_compile] cp={cp}\n{res}");
        }
        let _ = std::fs::remove_dir_all(&root);
        return None;
    }
    let mut classes: Vec<(String, Vec<u8>)> = Vec::new();
    collect_class_files(&outdir, &outdir, &mut classes)?;
    Some((outdir, classes))
}

/// Recursively gather `.class` files under `dir`, naming each by its path relative to `root` minus
/// the extension (JVM binary name with `/` separators).
fn collect_class_files(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Option<()> {
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            collect_class_files(root, &path, out)?;
        } else if path.extension().is_some_and(|e| e == "class") {
            let rel = path.strip_prefix(root).ok()?;
            let name = rel
                .with_extension("")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            out.push((name, std::fs::read(&path).ok()?));
        }
    }
    Some(())
}

/// Byte-compare one class emitted by krusty and kotlinc.
#[allow(dead_code)]
pub fn byte_diff_against_kotlinc(name: &str, src: &str, class: &str) -> Option<Result<(), String>> {
    let dir = scratch_dir()?;
    let kref = dir.join("ref");
    std::fs::create_dir_all(&kref).ok()?;
    let src_path = dir.join(format!("{name}.kt"));
    std::fs::write(&src_path, src).ok()?;
    let args = vec![
        "-d".to_string(),
        kref.to_string_lossy().into_owned(),
        src_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = kotlinc_compile(&args)?;
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let ref_bytes = std::fs::read(kref.join(format!("{class}.class"))).ok()?;

    let classes = compile_in_process_metadata_cp(src, name, &[])
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    let (_, krusty_bytes) = classes
        .iter()
        .find(|(n, _)| n == class)
        .unwrap_or_else(|| panic!("{name}: krusty did not emit {class}"));

    let _ = std::fs::remove_dir_all(&dir);
    if krusty_bytes == &ref_bytes {
        return Some(Ok(()));
    }
    let off = krusty_bytes
        .iter()
        .zip(ref_bytes.iter())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| krusty_bytes.len().min(ref_bytes.len()));
    Some(Err(format!(
        "{name}/{class}: bytes differ at offset {off} (krusty {} B, kotlinc {} B)",
        krusty_bytes.len(),
        ref_bytes.len()
    )))
}

#[cfg(test)]
mod scratch_tests {
    #[test]
    fn own_pid_is_not_dead() {
        assert!(!super::temp_dir_owner_is_dead(std::process::id() as i32));
    }

    #[test]
    fn reaped_pid_is_dead() {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe { libc::_exit(0) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(super::temp_dir_owner_is_dead(pid));
    }

    #[test]
    fn scratch_dirs_live_under_the_private_root() {
        let dir = super::scratch_dir().expect("allocate scratch directory");
        let root = dir.parent().expect("scratch dir has a root");
        assert_eq!(root.parent(), Some(super::scratch_namespace().as_path()));
        assert_eq!(
            super::scratch_owner_pid(root.file_name().unwrap()),
            Some(std::process::id() as i32)
        );
        assert!(dir.is_dir());
    }

    #[test]
    fn scratch_owner_accepts_only_reserved_root_names() {
        use std::ffi::OsStr;

        assert_eq!(super::scratch_owner_pid(OsStr::new("123")), Some(123));
        assert_eq!(super::scratch_owner_pid(OsStr::new("123_4")), Some(123));
        assert_eq!(super::scratch_owner_pid(OsStr::new("123_bad")), None);
        assert_eq!(super::scratch_owner_pid(OsStr::new("123_")), None);
    }

    #[test]
    fn stale_sweep_is_limited_to_private_roots() {
        assert_eq!(
            super::stale_scratch_roots(),
            [
                super::scratch_namespace(),
                std::env::temp_dir().join("krusty_scratch"),
            ]
        );
    }

    #[test]
    fn concurrent_scratch_allocations_are_unique() {
        const THREADS: usize = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
        let dirs = std::thread::scope(|scope| {
            let handles = (0..THREADS)
                .map(|_| {
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        super::scratch_dir().expect("allocate scratch directory")
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("allocator thread panicked"))
                .collect::<Vec<_>>()
        });

        let unique = dirs.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), THREADS);
        assert!(dirs.iter().all(|dir| dir.parent() == dirs[0].parent()));
        assert!(dirs.iter().all(|dir| dir.is_dir()));
        assert!(dirs.iter().all(|dir| dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.parse::<u64>().is_ok())));
    }
}
