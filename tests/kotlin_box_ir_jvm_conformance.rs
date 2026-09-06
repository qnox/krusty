//! Kotlin compiler conformance suite (`compiler/testData/codegen/box`).
//!
//! Each corpus case either passes or fails. Unsupported syntax, resolution, lowering, emission, or
//! harness behavior is a failure, never an exclusion from the denominator.
//!
//! Performance design:
//!   - In-process compilation (no krusty subprocess)
//!   - Rayon parallel compilation across all CPU cores
//!   - One persistent JVM runner per rayon thread (no per-test JVM restarts)
//!   - No javac: the runner loads bytes with a per-test ClassLoader + reflection
//!
//! Env vars:
//!   KRUSTY_REF_JAVA_HOME / JAVA_HOME
//!   KRUSTY_BOX_LIMIT        cap on files scanned (default: all)
//! The kotlin-stdlib jar is located from local caches (`common::stdlib_jar`) and supplied via
//! `-classpath` only to `// WITH_STDLIB` tests, plus the JVM runner's runtime classpath.

use std::fs;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::classreader::parse_class;

use super::common;

// BoxRunner.java source embedded at compile time; compiled once at test start.
const BOX_RUNNER_SRC: &str = r#"
import java.io.*;
import java.util.concurrent.*;

public class BoxRunner {
    static final long TIMEOUT_MS = 2000; // 2s per test
    static final ExecutorService EXEC = Executors.newCachedThreadPool(r -> {
        Thread t = new Thread(r);
        t.setDaemon(true);
        return t;
    });

    public static void main(String[] args) throws Exception {
        DataInputStream din = new DataInputStream(new BufferedInputStream(System.in, 65536));
        DataOutputStream dout = new DataOutputStream(new BufferedOutputStream(System.out, 4096));
        // Redirect System.out so test code (e.g. println) can't corrupt the protocol pipe.
        // Capture dout before the redirect so our own writes still go to the real stdout.
        System.setOut(System.err);
        while (true) {
            int n;
            try { n = din.readInt(); } catch (EOFException e) { break; }
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
            final String[] namesF = names;
            final byte[][] dataF = data;
            final String boxClassF = boxClass;
            Future<String> future = EXEC.submit(() -> {
                try {
                    TestClassLoader ldr = new TestClassLoader(namesF, dataF);
                    Class<?> cls = ldr.loadClass(boxClassF);
                    String r = (String) cls.getMethod("box").invoke(null);
                    return r == null ? "null" : r;
                } catch (Throwable t) {
                    Throwable cause = (t instanceof java.lang.reflect.InvocationTargetException && t.getCause() != null) ? t.getCause() : t;
                    return "ERROR:" + cause.getClass().getSimpleName() + ":" + cause.getMessage();
                }
            });
            String result;
            try {
                result = future.get(TIMEOUT_MS, TimeUnit.MILLISECONDS);
            } catch (TimeoutException e) {
                future.cancel(true);
                result = "ERROR:TimeoutException:box() exceeded " + TIMEOUT_MS + "ms";
            } catch (ExecutionException e) {
                result = "ERROR:" + e.getCause().getClass().getSimpleName() + ":" + e.getCause().getMessage();
            }
            byte[] rb = result.getBytes("UTF-8");
            dout.writeInt(rb.length);
            dout.write(rb);
            dout.flush();
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

// Backend-applicability, classpath directives, and module splitting are the SINGLE source of truth in
// `krusty::conformance` (shared with the `survey` bin so the two never drift).
use krusty::conformance::{backend_applicable, frontend_applicable, split_modules};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn discover_box_dir() -> PathBuf {
    if let Some(path) = krusty::toolchain::box_corpus_dir() {
        return path;
    }

    let out = Command::new("just")
        .arg("box-corpus")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap_or_else(|e| {
            panic!("failed to provision Kotlin box corpus via `just box-corpus`: {e}")
        });
    if !out.status.success() {
        panic!(
            "`just box-corpus` failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    if path.is_dir() {
        path
    } else {
        panic!(
            "`just box-corpus` returned a non-existent path: {}",
            path.display()
        );
    }
}

// Sub-phase timers (ns, accumulated across all files and threads).
static T_LEX: AtomicU64 = AtomicU64::new(0);
static T_PARSE: AtomicU64 = AtomicU64::new(0);
static T_SIGS: AtomicU64 = AtomicU64::new(0);
static T_CHECK: AtomicU64 = AtomicU64::new(0);
static T_EMIT: AtomicU64 = AtomicU64::new(0);

/// Peak resident-set size for the current process in KiB.
///
/// `getrusage` reports bytes on macOS and KiB on the other Unix targets supported by the test
/// harness. Keeping this measurement in the harness avoids exposing an OS process-inspection API
/// from the JVM classpath provider merely to support profiling.
#[cfg(unix)]
fn peak_process_rss_kb() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` points to writable storage for exactly one `rusage`; `getrusage` initializes
    // it on success, which is checked before `assume_init`.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: a successful `getrusage` call initialized the complete value.
    let bytes_or_kib = u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).ok()?;
    #[cfg(target_os = "macos")]
    {
        Some(bytes_or_kib.div_ceil(1024))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(bytes_or_kib)
    }
}

#[cfg(not(unix))]
fn peak_process_rss_kb() -> Option<u64> {
    None
}

#[derive(Clone)]
struct ActiveBoxCase {
    path: PathBuf,
    phase: String,
    since: Instant,
}

fn mark_box_case_phase(
    active: &[Mutex<Option<ActiveBoxCase>>],
    worker: usize,
    path: &Path,
    phase: &str,
) {
    *active[worker].lock().unwrap() = Some(ActiveBoxCase {
        path: path.to_path_buf(),
        phase: phase.to_string(),
        since: Instant::now(),
    });
}

// Compile Kotlin source to a list of (class_internal_name, class_bytes) pairs.
// Returns None if compilation fails (unsupported feature).
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

fn compile_source(
    src: &str,
    stem: &str,
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
    progress: &dyn Fn(&str),
) -> Option<Vec<(String, Vec<u8>)>> {
    let mut diags = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(src);
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
    let cp = harness_classpath(cp_paths);
    progress("two-pass FIR");
    let started = std::time::Instant::now();
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let inputs = [krusty::source::SourceInput::kotlin(src).with_file_stem(stem)];
    let stems = [stem.to_string()];
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    let backend = krusty::jvm::JvmBackend::new(cp)
        .with_jvm_default(krusty::conformance::jvm_default_mode(src));
    let outputs = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diags);
    T_EMIT.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);

    let classes = outputs
        .into_iter()
        .filter_map(|(path, bytes)| {
            path.strip_suffix(".class")
                .map(|internal| (internal.to_string(), bytes))
        })
        .collect::<Vec<_>>();
    (!diags.has_errors() && !classes.is_empty()).then_some(classes)
}

/// The `helpers` package source the Kotlin test infra injects into every `// WITH_COROUTINES` box test
/// (kotlinc's `TestFiles.java` adds a `CoroutineUtil.kt` whose text is
/// `TestHelperGenerator.createTextForCoroutineHelpers(checkStateMachine, checkTailCallOptimization)`).
/// This is the `false, false` variant — the box corpus uses NEITHER `CHECK_STATE_MACHINE` nor
/// `CHECK_TAIL_CALL_OPTIMIZATION` (verified: 0 files), so the state-machine/tail-call checker classes are
/// never emitted. These helpers live in `kotlin.coroutines.*` (the stdlib), NOT `kotlinx-coroutines-core`
/// — no box test imports `kotlinx.coroutines`. Compiled as an extra source file in the same module so
/// `EmptyContinuation`, `runBlocking`, `handleResultContinuation`, … resolve exactly as under kotlinc.
const COROUTINE_HELPERS: &str = r#"package helpers
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

fun <T> runBlocking(block: suspend () -> T): T {
    var res: Result<T>? = null
    block.startCoroutine(Continuation(EmptyCoroutineContext) {
        res = it
    })
    return res!!.getOrThrow()
}

fun <T> handleResultContinuation(x: (T) -> Unit): Continuation<T> = object: Continuation<T> {
    override val context = EmptyCoroutineContext
    override fun resumeWith(result: Result<T>) {
       x(result.getOrThrow())
    }
}

fun handleExceptionContinuation(x: (Throwable) -> Unit): Continuation<Any?> = object: Continuation<Any?> {
    override val context = EmptyCoroutineContext
    override fun resumeWith(result: Result<Any?>) {
       result.exceptionOrNull()?.let(x)
    }
}

open class EmptyContinuation(override val context: CoroutineContext = EmptyCoroutineContext) : Continuation<Any?> {
    companion object : EmptyContinuation()
    override fun resumeWith(result: Result<Any?>) {
       result.getOrThrow()
    }
}

class ResultContinuation : Continuation<Any?> {
    override val context = EmptyCoroutineContext
    override fun resumeWith(result: Result<Any?>) {
       this.result = result.getOrThrow()
    }

    var result: Any? = null
}
"#;

/// Compile a `// FILE: name.kt`-split multi-file test as ONE module: parse each block, collect global
/// signatures, populate the cross-file function→facade map (`SymbolTable.fn_facades`, like the CLI
/// driver), then type-check + lower + emit each file, returning ALL classes. Returns `None` if any file
/// uses something the IR backend can't lower (e.g. a cross-file *class* reference — only cross-file
/// top-level functions are modeled so far), so the test SKIPS rather than miscompiles.
///
/// `// WITH_COROUTINES` tests are routed here too (even single-file): the generated `helpers` source is
/// appended as an extra block, mirroring kotlinc's `CoroutineUtil.kt` injection.
fn compile_multifile(
    src: &str,
    main_stem: &str,
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<Vec<(String, Vec<u8>)>> {
    // Split on `// FILE: name.kt` markers (the preamble before the first marker is directives).
    // `.java` blocks are collected separately: javac compiles them first (in-process, via the
    // persistent JavaRunner) and their classes join krusty's classpath as an in-memory overlay,
    // mirroring how kotlinc's test infra makes Java sources visible to the Kotlin compile.
    let (mut blocks, java_blocks) = krusty::conformance::split_files(src);
    // Single-file (no `// FILE:` markers) but routed here for coroutine-helper injection: the whole
    // source is the one main block.
    if blocks.is_empty() && java_blocks.is_empty() {
        blocks.push((main_stem.to_string(), src.to_string()));
    }
    // Mirror kotlinc: a `// WITH_COROUTINES` test gets the generated `helpers` source as an extra file.
    if src.contains("// WITH_COROUTINES") {
        blocks.push(("CoroutineUtil".to_string(), COROUTINE_HELPERS.to_string()));
    }
    if blocks.is_empty() || (blocks.len() < 2 && java_blocks.is_empty()) {
        return None; // not actually multi-file (and nothing for javac either)
    }

    // Compile the Java blocks first (in-process javac, persistent JVM — no per-test spawn); krusty
    // sees the emitted classes as an overlay on the shared jar classpath, and they ride along to
    // BoxRunner's loader. When javac-first fails — the Java references Kotlin declarations, so it
    // cannot be ordered first — fall back to the KOTLIN-FIRST pipeline: signature stubs → krusty →
    // real javac (docs/JAVA_INTEROP.md slice 2).
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
            None => return compile_kotlin_first(src, &blocks, &java_blocks, cp_jars, jdk_modules),
        }
    }

    // `// LANGUAGE:` directives live in the preamble before the first `// FILE:` — read them from the
    // whole source and apply to every block.
    let features = krusty::features::LangFeatures::from_source(src);
    let compiled = compile_blocks(
        &blocks,
        cp_jars,
        &[],
        jdk_modules,
        &features,
        None,
        &java_classes,
    );
    let mut out = compiled?;
    out.extend(java_classes);
    Some(out)
}

/// Kotlin-first mixed compilation for a test whose Java references Kotlin declarations. Kotlin and
/// Java files enter the production frontend together, so its JVM provider publishes the Java
/// declaration headers in Pass 1. Real javac runs only after Kotlin emission and only its classes
/// ship. Header installation or javac failure means the corpus case failed to compile.
fn compile_kotlin_first(
    src: &str,
    blocks: &[(String, String)],
    java_blocks: &[(String, String)],
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<Vec<(String, Vec<u8>)>> {
    let features = krusty::features::LangFeatures::from_source(src);
    let kotlin_classes = compile_blocks_mixed(
        blocks,
        java_blocks,
        cp_jars,
        &[],
        jdk_modules,
        &features,
        None,
        &[],
    )?;

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
        let (javadir, java_classes) = common::javac_compile(java_blocks, &javac_cp)?;
        if let Some(jroot) = javadir.parent() {
            let _ = fs::remove_dir_all(jroot);
        }
        let mut out = kotlin_classes.clone();
        out.extend(java_classes);
        Some(out)
    })();
    let _ = fs::remove_dir_all(&root);
    result
}

/// Compile a set of already-split source blocks `(stem, content)` as ONE krusty module against the
/// given classpath through the production two-pass FIR driver, returning all emitted classes.
fn compile_blocks(
    blocks: &[(String, String)],
    cp_jars: &[std::path::PathBuf],
    friend_paths: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
    features: &krusty::features::LangFeatures,
    progress: Option<&dyn Fn(&str)>,
    overlay: &[(String, Vec<u8>)],
) -> Option<Vec<(String, Vec<u8>)>> {
    compile_blocks_mixed(
        blocks,
        &[],
        cp_jars,
        friend_paths,
        jdk_modules,
        features,
        progress,
        overlay,
    )
}

fn compile_blocks_mixed(
    blocks: &[(String, String)],
    java_blocks: &[(String, String)],
    cp_jars: &[std::path::PathBuf],
    friend_paths: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
    features: &krusty::features::LangFeatures,
    progress: Option<&dyn Fn(&str)>,
    overlay: &[(String, Vec<u8>)],
) -> Option<Vec<(String, Vec<u8>)>> {
    let report = |phase: &str| {
        if let Some(progress) = progress {
            progress(phase);
        }
    };
    let mut diags = DiagSink::new();
    report("module classpath");
    let mut cp_paths: Vec<std::path::PathBuf> = cp_jars.to_vec();
    if let Some(p) = jdk_modules {
        cp_paths.push(p.to_path_buf());
    }
    let cp = harness_classpath_with_friends(cp_paths, friend_paths.to_vec());
    let _overlay = OverlayGuard::set(&cp, overlay);
    report("module two-pass FIR");
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let stems: Vec<String> = blocks
        .iter()
        .chain(java_blocks)
        .map(|(name, _)| name.clone())
        .collect();
    let mut inputs = blocks
        .iter()
        .map(|(stem, content)| krusty::source::SourceInput::kotlin(content).with_file_stem(stem))
        .collect::<Vec<_>>();
    inputs.extend(
        java_blocks
            .iter()
            .map(|(stem, content)| krusty::source::SourceInput::java(content).with_file_stem(stem)),
    );
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, features, &mut diags,
    );
    let jvm_default = blocks
        .iter()
        .map(|(_, source)| krusty::conformance::jvm_default_mode(source))
        .find(|mode| *mode != krusty::jvm::ir_emit::JvmDefaultMode::default())
        .unwrap_or_default();
    let backend = krusty::jvm::JvmBackend::new(cp).with_jvm_default(jvm_default);
    let outputs = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diags);
    let classes = outputs
        .into_iter()
        .filter_map(|(path, bytes)| {
            path.strip_suffix(".class")
                .map(|internal| (internal.to_string(), bytes))
        })
        .collect::<Vec<_>>();
    (!diags.has_errors() && !classes.is_empty()).then_some(classes)
}

/// Write emitted `(internal_name, bytes)` classes under `dir` as `internal/name.class` (package dirs
/// created), so a later module's compile can read them off the classpath. `None` on any I/O error.
fn write_classes_to_dir(classes: &[(String, Vec<u8>)], dir: &Path) -> Option<()> {
    for (name, bytes) in classes {
        let path = dir.join(format!("{name}.class"));
        fs::create_dir_all(path.parent()?).ok()?;
        fs::write(&path, bytes).ok()?;
    }
    Some(())
}

/// Compile a `// MODULE:` test the way a Gradle multi-module build (or kotlinc's separate-compilation
/// test mode) does — but with EVERY module compiled by KRUSTY, so this also exercises krusty's own
/// `@Metadata` WRITE→READ round-trip across a real classpath boundary. Each module is compiled in
/// declaration (dependency) order against its dependency modules' EMITTED classes on the classpath; all
/// modules' classes are returned together so the persistent BoxRunner loads them in one classloader and
/// runs `box()`. `None` (SKIP) if the shape is unsupported or any module fails to lower.
fn compile_module_test(
    src: &str,
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
    progress: &dyn Fn(&str),
) -> Option<Vec<(String, Vec<u8>)>> {
    static UID: AtomicU64 = AtomicU64::new(0);
    let mut modules = split_modules(src)?;
    // kotlinc's `// WITH_COROUTINES` helpers live in an implicit `support` module every module sees.
    if krusty::conformance::directive(src, "WITH_COROUTINES") {
        krusty::conformance::inject_support_module(&mut modules, COROUTINE_HELPERS);
    }
    let features = krusty::features::LangFeatures::from_source(src);
    let uid = UID.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("krusty_modtest_{}_{uid}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let mut dirmap: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    let mut all: Vec<(String, Vec<u8>)> = Vec::new();
    let mut ok = true;
    // Build units: a `dependsOn` target compiles INTO its dependents (kotlinc's JVM MPP model — the
    // platform module and its dependsOn chain are ONE compilation), so each unit's sources already
    // carry the chain dependency-first.
    for m in &krusty::conformance::module_units(&modules) {
        // Compile-time classpath = the base (stdlib/JDK) + each dependency module's emitted-class
        // dir. FRIEND deps ride the same classpath (their `internal` visibility is the friend
        // part; krusty resolves them like any dependency).
        let mut cp = cp_jars.to_vec();
        let mut friend_paths = Vec::new();
        for d in &m.deps {
            match dirmap.get(d) {
                Some(p) => {
                    cp.push(p.clone());
                    if m.friends.iter().any(|friend| friend == d) {
                        friend_paths.push(p.clone());
                    }
                }
                None => {
                    ok = false; // a dependency declared out of order / on an unbuilt module — skip
                    break;
                }
            }
        }
        if !ok {
            break;
        }
        let (files, java_files) = (&m.files, &m.java_files);
        let report = |phase: &str| progress(&format!("module {}: {phase}", m.name));
        // A source-less unit (an empty hmpp intermediate built standalone) emits nothing; it still
        // gets a (created, empty) classpath dir so dependents resolve it.
        if files.is_empty() && java_files.is_empty() {
            let moddir = tmp.join(&m.name);
            if fs::create_dir_all(&moddir).is_err() {
                ok = false;
                break;
            }
            dirmap.insert(m.name.clone(), moddir);
            continue;
        }
        // A module's `.java` sources: javac-first against the module classpath (Java referencing
        // only deps/JDK); when that fails — the Java references THIS module's Kotlin — fall back to
        // the Kotlin-first stub pipeline, exactly like the single-module path.
        let classes = if java_files.is_empty() {
            compile_blocks(
                files,
                &cp,
                &friend_paths,
                jdk_modules,
                &features,
                Some(&report),
                &[],
            )
        } else {
            match common::javac_compile(java_files, &cp) {
                Some((javadir, java_classes)) => {
                    if let Some(root) = javadir.parent() {
                        let _ = fs::remove_dir_all(root);
                    }
                    let kotlin = if files.is_empty() {
                        Some(Vec::new()) // a Java-only module
                    } else {
                        // The javac classes ride the overlay (Java classes carry no Kotlin
                        // facades, so the class-lookup channels the overlay feeds suffice). The
                        // Kotlin dependency DIRS stay on the classpath: dependency-module
                        // top-level functions resolve through the package catalog, which only
                        // directory/jar entries contribute to — an overlaid dependency module
                        // loses them (measured: -95 box passes).
                        compile_blocks(
                            files,
                            &cp,
                            &friend_paths,
                            jdk_modules,
                            &features,
                            None,
                            &java_classes,
                        )
                    };
                    kotlin.map(|mut k| {
                        k.extend(java_classes);
                        k
                    })
                }
                None => compile_kotlin_first(src, files, java_files, &cp, jdk_modules),
            }
        };
        let Some(classes) = classes else {
            ok = false;
            break;
        };
        let moddir = tmp.join(&m.name);
        if write_classes_to_dir(&classes, &moddir).is_none() {
            ok = false;
            break;
        }
        dirmap.insert(m.name.clone(), moddir);
        all.extend(classes);
    }
    // The dependency classes were read off disk during compilation and are now all held in `all` (for the
    // in-memory BoxRunner), so the scratch dir is no longer needed.
    let _ = fs::remove_dir_all(&tmp);
    (ok && !all.is_empty()).then_some(all)
}

/// Find the class that declares `static box()Ljava/lang/String;`.
fn find_box_class(classes: &[(String, Vec<u8>)]) -> Option<String> {
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

/// Read exactly `buf.len()` bytes from `fd`, aborting after `deadline`.
///
/// Uses `poll(2)` so we never block permanently on an unresponsive JVM — the
/// Java-side `Future.get(2000ms)` is only a best-effort guard; if the JVM's
/// main thread itself stalls we still need an OS-level escape hatch.
fn read_exact_deadline(fd: i32, buf: &mut [u8], deadline: Instant) -> std::io::Result<()> {
    let mut pos = 0;
    while pos < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "BoxRunner read timeout",
            ));
        }
        // poll(2) to wait up to `remaining` for data, but cap at 1 s so we
        // re-check the deadline even if remaining is very large.
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
            continue; // loop back and re-check deadline
        }
        let n = unsafe {
            libc::read(
                fd,
                buf[pos..].as_mut_ptr() as *mut libc::c_void,
                (buf.len() - pos) as libc::size_t,
            )
        };
        match n {
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
                    "BoxRunner EOF",
                ));
            }
            n => pos += n as usize,
        }
    }
    Ok(())
}

/// A persistent JVM subprocess that accepts class bytes and runs box().
struct BoxRunner {
    _child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl BoxRunner {
    fn new(java: &str, runner_cp: &str, stdlib: &str) -> Self {
        let cp = if stdlib.is_empty() {
            runner_cp.to_string()
        } else {
            format!("{runner_cp}:{stdlib}")
        };
        let mut child = Command::new(java)
            .args(["-cp", &cp, "BoxRunner"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to launch BoxRunner JVM");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        BoxRunner {
            _child: child,
            stdin,
            stdout,
        }
    }

    /// Send class bytes and box class name; return the result string.
    /// Returns `None` if the BoxRunner subprocess died (caller should restart it).
    fn run(&mut self, classes: &[(String, Vec<u8>)], box_class: &str) -> Option<String> {
        self.try_run(classes, box_class).ok()
    }

    fn try_run(
        &mut self,
        classes: &[(String, Vec<u8>)],
        box_class: &str,
    ) -> std::io::Result<String> {
        // Write: [u32 n][for each: u16 name_len, name, u32 data_len, data][u16 box_len, box_name]
        let n = classes.len() as u32;
        self.stdin.write_all(&n.to_be_bytes())?;
        for (name, data) in classes {
            let nl = name.len() as u16;
            self.stdin.write_all(&nl.to_be_bytes())?;
            self.stdin.write_all(name.as_bytes())?;
            let dl = data.len() as u32;
            self.stdin.write_all(&dl.to_be_bytes())?;
            self.stdin.write_all(data)?;
        }
        let bl = box_class.len() as u16;
        self.stdin.write_all(&bl.to_be_bytes())?;
        self.stdin.write_all(box_class.as_bytes())?;
        self.stdin.flush()?;

        // Read: [u32 result_len][result_bytes]
        // Hard deadline: Java allows 2 s per test; give 10 s total for the round-trip.
        // This is our OS-level escape hatch in case the JVM's main thread stalls.
        let deadline = Instant::now() + Duration::from_secs(10);
        let fd = self.stdout.as_raw_fd();
        let mut len_buf = [0u8; 4];
        read_exact_deadline(fd, &mut len_buf, deadline)?;
        let rlen = u32::from_be_bytes(len_buf) as usize;
        let mut result = vec![0u8; rlen];
        read_exact_deadline(fd, &mut result, deadline)?;
        Ok(String::from_utf8_lossy(&result).into_owned())
    }
}

/// Compile and run BoxRunner.java once, return path to the directory with BoxRunner.class.
fn setup_runner(java_home: &str, _work: &Path) -> PathBuf {
    // Cache the compiled runner in a stable location keyed by the source hash — BoxRunner.java is
    // static, so javac runs once across all test runs, not every invocation (~1.8s saved per run).
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in BOX_RUNNER_SRC.bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let runner_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/box_runner_{hash:016x}"));
    if runner_dir.join("BoxRunner.class").is_file() {
        return runner_dir; // already compiled
    }
    fs::create_dir_all(&runner_dir).unwrap();
    let src_path = runner_dir.join("BoxRunner.java");
    fs::write(&src_path, BOX_RUNNER_SRC).unwrap();
    let javac = format!("{java_home}/bin/javac");
    let out = Command::new(&javac)
        .args([
            "-source",
            "8",
            "-target",
            "8",
            "-d",
            runner_dir.to_str().unwrap(),
        ])
        .arg(&src_path)
        .output()
        .expect("javac failed to launch");
    if !out.status.success() {
        panic!(
            "BoxRunner.java compile failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    runner_dir
}

fn conformance_report(scanned: usize, passed: usize) -> String {
    let pct = if scanned == 0 {
        0.0
    } else {
        100.0 * passed as f64 / scanned as f64
    };
    format!("{pct:.1} {passed} {scanned}\n")
}

#[test]
fn conformance_report_has_stable_machine_format() {
    assert_eq!(conformance_report(7352, 3064), "41.7 3064 7352\n");
    assert_eq!(conformance_report(0, 0), "0.0 0 0\n");
}

/// Base frame of a folded stack: the sampled thread's name, or its id when it has none.
fn profile_thread_label(name: &str, id: u64) -> String {
    if name.is_empty() {
        id.to_string()
    } else {
        name.to_string()
    }
}

/// One inferno-folded line: `thread;root;…;leaf count`. `leaf_first` is the sampled stack in
/// backtrace order (innermost frame first), which the line reverses into root-first order.
fn profile_folded_line(thread_label: &str, leaf_first: &[&str], count: isize) -> String {
    let mut line = String::from(thread_label);
    for name in leaf_first.iter().rev() {
        line.push(';');
        line.push_str(name);
    }
    line.push(' ');
    line.push_str(&count.to_string());
    line
}

#[test]
fn profile_folds_stacks_root_first() {
    assert_eq!(
        profile_folded_line("worker", &["leaf", "middle", "root"], 12),
        "worker;root;middle;leaf 12"
    );
    // A stack sampled with no resolvable frame still carries its thread and count.
    assert_eq!(profile_folded_line("7", &[], 3), "7 3");
    assert_eq!(profile_thread_label("", 42), "42");
    assert_eq!(profile_thread_label("worker", 42), "worker");
}

/// Symbolize the sampled profile, write `target/flamegraph.svg`, and print the hotspot table.
///
/// pprof's own `report().build()` resolves and demangles every frame of every distinct stack. A
/// full-corpus profile holds ~10^5 distinct stacks over only a few thousand distinct instruction
/// pointers, so that is ~10^7 single-threaded symbol lookups — many minutes, well past the harness
/// deadline, which kills the run and leaves a 0-byte SVG behind. Resolve each instruction pointer
/// once into a memo and fold the stacks out of that instead.
fn write_flamegraph(guard: &pprof::ProfilerGuard<'_>) {
    /// One instruction pointer's symbols: several when the compiler inlined into it.
    struct Resolved {
        /// Demangled names, innermost inlined frame first.
        names: Vec<String>,
        /// pprof's own sampling handler; it and the frame below it are not part of the workload.
        signal_handler: bool,
    }

    // Drop any SVG an earlier run left behind before doing anything that can bail out, so a run that
    // writes no flamegraph cannot leave a stale one to be read as this run's profile.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/flamegraph.svg");
    let _ = std::fs::remove_file(&path);

    let report = match guard.report().build_unresolved() {
        Ok(report) => report,
        Err(e) => {
            eprintln!("profiler: could not read the sampled profile: {e}");
            return;
        }
    };

    let t0 = Instant::now();
    let mut memo: std::collections::HashMap<usize, Resolved> = std::collections::HashMap::new();
    for frames in report.data.keys() {
        for frame in &frames.frames {
            let ip = frame.ip() as usize;
            if memo.contains_key(&ip) {
                continue;
            }
            let mut names: Vec<String> = Vec::new();
            backtrace::resolve_frame(frame, |symbol| {
                // `{:#}` demangles and drops the trailing `::h<hash>`, so one function reached from
                // different stacks folds into a single flamegraph frame.
                names.push(match symbol.name() {
                    Some(name) => format!("{name:#}"),
                    None => "unknown".to_string(),
                });
            });
            let signal_handler = names
                .iter()
                // macOS prepends an underscore even to `#[no_mangle]` symbols.
                .any(|n| n == "perf_signal_handler" || n == "_perf_signal_handler");
            memo.insert(
                ip,
                Resolved {
                    names,
                    signal_handler,
                },
            );
        }
    }
    let symbolize_ms = t0.elapsed().as_millis();

    let mut folded: Vec<String> = Vec::with_capacity(report.data.len());
    let mut leaf: std::collections::HashMap<&str, isize> = std::collections::HashMap::new();
    let mut total: isize = 0;
    for (frames, count) in &report.data {
        total += *count;
        let mut stack: Vec<&str> = Vec::new();
        let mut frame_iter = frames.frames.iter();
        while let Some(frame) = frame_iter.next() {
            let Some(resolved) = memo.get(&(frame.ip() as usize)) else {
                continue;
            };
            if resolved.signal_handler {
                frame_iter.next();
                continue;
            }
            stack.extend(resolved.names.iter().map(String::as_str));
        }
        // Terminal-readable hotspots: aggregate samples by the innermost krusty frame. The raw leaf
        // is usually the sampling/backtrace/thread trampoline; the SVG keeps the full stack.
        if let Some(name) = stack
            .iter()
            .find(|n| n.starts_with("krusty::") || n.starts_with("<krusty::"))
        {
            *leaf.entry(name).or_default() += *count;
        }
        let thread = String::from_utf8_lossy(&frames.thread_name[..frames.thread_name_length]);
        folded.push(profile_folded_line(
            &profile_thread_label(&thread, frames.thread_id),
            &stack,
            *count,
        ));
    }

    if folded.is_empty() {
        eprintln!("profiler: no samples collected, no flamegraph written");
        return;
    }
    match std::fs::File::create(&path) {
        Ok(f) => {
            let mut options = pprof::flamegraph::Options::default();
            match pprof::flamegraph::from_lines(
                &mut options,
                folded.iter().map(|s| s.as_str()),
                f,
            ) {
                Ok(()) => eprintln!(
                    "flamegraph written to {} ({} stacks over {} addresses, symbolized in {symbolize_ms}ms)",
                    path.display(),
                    folded.len(),
                    memo.len()
                ),
                Err(e) => eprintln!("profiler: flamegraph write failed: {e}"),
            }
        }
        Err(e) => eprintln!("profiler: cannot create {}: {e}", path.display()),
    }

    let mut v: Vec<_> = leaf.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("--- profiler: top krusty frames ({total} samples) ---");
    for (name, c) in v.into_iter().take(25) {
        eprintln!("  {:>5.1}%  {name}", 100.0 * c as f64 / total.max(1) as f64);
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

#[test]
#[cfg(unix)]
fn peak_process_rss_is_observable() {
    assert!(
        peak_process_rss_kb().is_some_and(|rss| rss > 0),
        "the memory profiler must not silently report zero RSS on a supported host"
    );
}

#[test]
fn kotlin_codegen_box_conformance() {
    eprintln!("box setup: discover corpus");
    let box_dir = discover_box_dir();
    eprintln!("box setup: corpus={}", box_dir.display());
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping box conformance: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    // The JDK bootclasspath as an explicit `-classpath` entry: the running JDK's `lib/modules`
    // jimage, so JDK types (`StringBuilder`, …) resolve like any classpath type.
    let jdk_modules: Option<std::path::PathBuf> = {
        let p = Path::new(&java_home).join("lib").join("modules");
        p.is_file().then_some(p)
    };
    // Locate a real kotlin-stdlib jar (drop-in `-classpath`), used for `// WITH_STDLIB` tests at
    // compile time and on the JVM at runtime. No bespoke env var.
    eprintln!("box setup: locate Kotlin runtime jars");
    let stdlib_jar = common::stdlib_jar();
    // Runtime classpath: every candidate stdlib-family jar (kotlin-stdlib, kotlin-test, reflect,
    // stdlib-jdk8, coroutines, annotations). The per-thread JVM has a fixed classpath, and extra
    // jars are harmless to tests that don't use them; the *compile* classpath stays directive-exact.
    let stdlib = {
        let mut paths: Vec<String> = Vec::new();
        for p in [
            Some(stdlib_jar.clone()),
            common::kotlin_test_jar(),
            common::dist_jar("kotlin-reflect.jar")
                .or_else(|| common::find_jar("kotlin-reflect-", &["sources"])),
            common::find_jar("kotlin-stdlib-jdk8", &[]),
            common::find_jar("kotlinx-coroutines-core", &["jdk8"]),
            common::find_jar("annotations-", &[]),
        ]
        .into_iter()
        .flatten()
        {
            paths.push(p.to_string_lossy().into_owned());
        }
        paths.join(":")
    };
    eprintln!("box setup: Kotlin runtime jars ready");
    let limit: usize = env("KRUSTY_BOX_LIMIT")
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);

    let mut files = krusty::conformance::kotlin_files(&box_dir);
    // KRUSTY_BOX_ONLY: run only files whose path contains this substring — a focused single-test debug
    // loop (pair with a `trace`-feature build + KRUSTY_TRACE=<category>). Empty/unset runs the corpus.
    if let Some(only) = env("KRUSTY_BOX_ONLY") {
        files.retain(|f| f.to_string_lossy().contains(&only));
    }
    // KRUSTY_BOX_LIMIT caps the run for fast dev rounds. Sample evenly across the *sorted* corpus
    // (a stride) rather than truncating to the first N — the first N are all `annotations/…`, which
    // would hide coverage in every other package. A full (unset) run keeps the whole corpus.
    files = krusty::conformance::evenly_sample(files, limit);
    eprintln!("box setup: scheduled {} cases", files.len());

    let work = std::env::temp_dir().join(format!("krusty_box_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    // Compile BoxRunner.java once.
    let runner_cp = setup_runner(&java_home, &work);
    let runner_cp_str = runner_cp.to_str().unwrap().to_string();
    eprintln!("box setup: JVM runner ready");

    // Build a thread pool with a large stack (8 MiB) so deeply-nested source files don't
    // overflow the default 2 MiB Rayon stack during recursive descent parsing/checking.
    let mut pb = rayon::ThreadPoolBuilder::new()
        // Generous worker stacks: the recursive-descent parser/lowering uses far more stack per frame in
        // an unoptimized (`--profile gate`) build than in `--release`, so a deeply nested test would
        // overflow an 8 MB stack. 64 MB keeps the fast-iteration profile robust.
        .stack_size(64 * 1024 * 1024);
    if let Some(n) = env("KRUSTY_TEST_THREADS").and_then(|v| v.parse::<usize>().ok()) {
        pb = pb.num_threads(n);
    }
    let pool = pb.build().unwrap();
    let n_threads = pool.current_num_threads();
    eprintln!("box setup: compiler pool ready ({n_threads} workers)");
    let runners: Vec<Mutex<Option<BoxRunner>>> = (0..n_threads).map(|_| Mutex::new(None)).collect();
    let completed = Arc::new(AtomicUsize::new(0));
    let recent_failures: Arc<Mutex<Vec<(PathBuf, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let active: Arc<Vec<Mutex<Option<ActiveBoxCase>>>> =
        Arc::new((0..n_threads).map(|_| Mutex::new(None)).collect());
    let progress_done = Arc::new(AtomicBool::new(false));
    let progress_thread = {
        let completed = completed.clone();
        let recent_failures = recent_failures.clone();
        let active = active.clone();
        let progress_done = progress_done.clone();
        let total = files.len();
        std::thread::spawn(move || {
            let started = Instant::now();
            eprintln!("box progress: 0/{total} (starting)");
            while !progress_done.load(Ordering::Relaxed) {
                std::thread::park_timeout(Duration::from_secs(10));
                if progress_done.load(Ordering::Relaxed) {
                    break;
                }
                let elapsed = started.elapsed();
                let done = completed.load(Ordering::Relaxed);
                let rate = done as f64 / elapsed.as_secs_f64().max(0.001);
                let mut slow: Vec<ActiveBoxCase> = active
                    .iter()
                    .filter_map(|slot| slot.lock().unwrap().clone())
                    .filter(|case| case.since.elapsed() >= Duration::from_secs(2))
                    .collect();
                slow.sort_by_key(|case| std::cmp::Reverse(case.since.elapsed()));
                eprintln!(
                    "box progress: {done}/{total} in {:.1}s ({rate:.1} cases/s)",
                    elapsed.as_secs_f64()
                );
                for case in slow.into_iter().take(16) {
                    eprintln!(
                        "  active {:.1}s [{}] {}",
                        case.since.elapsed().as_secs_f64(),
                        case.phase,
                        case.path.display()
                    );
                }
                for (path, why) in recent_failures.lock().unwrap().iter().rev().take(8) {
                    eprintln!("  recent failure {}: {why}", path.display());
                }
            }
        })
    };

    // Phase timers (nanoseconds, accumulated across threads).
    let t_compile = AtomicU64::new(0);
    let t_jvm = AtomicU64::new(0);
    let t_closure = AtomicU64::new(0);
    let t_read = AtomicU64::new(0);
    let t_cpjars = AtomicU64::new(0);
    let t_total_start = std::time::Instant::now();

    // Optional sampling profiler → flamegraph SVG (KRUSTY_FLAMEGRAPH=1). Captures all rayon worker
    // threads via SIGPROF; off by default so normal runs aren't perturbed.
    let flame_guard = if env("KRUSTY_FLAMEGRAPH").is_some() {
        Some(
            pprof::ProfilerGuardBuilder::default()
                .frequency(997)
                .blocklist(&["libc", "libgcc", "pthread", "vdso"])
                .build()
                .expect("start profiler"),
        )
    } else {
        None
    };

    let no_run = env("KRUSTY_NO_RUN").is_some();
    let byte_diff_on = env("KRUSTY_BYTE_DIFF").is_some();
    let byte_diffs: Mutex<Vec<(PathBuf, ByteDiff)>> = Mutex::new(Vec::new());

    // Heap profiler (`--features dhat-heap`): its `Drop` at end of scope writes `dhat-heap.json` with
    // bytes-alive-at-peak by allocation call stack. Pair with `KRUSTY_NO_RUN=1` to profile the compiler
    // alone, and `KRUSTY_BOX_LIMIT` to keep the (instrumented, slower) run short.
    #[cfg(feature = "dhat-heap")]
    let _dhat = krusty::dhat::Profiler::new_heap();

    // Parallel phase: compile each test in-process, run in the per-thread JVM.
    let results: Vec<(PathBuf, TestResult)> = pool.install(|| {
        files
            .par_iter()
            .map(|file| {
                let tid = rayon::current_thread_index().unwrap_or(0);
                mark_box_case_phase(&active, tid, file, "read");
                if env("KRUSTY_BOX_TRACE_CASES").is_some() {
                    eprintln!("box case {}", file.display());
                }
                let tc0 = std::time::Instant::now();
                let tr0 = std::time::Instant::now();
                let src = fs::read_to_string(file).unwrap_or_default();
                // The Kotlin test runner expands the `OPTIONAL_JVM_INLINE_ANNOTATION` placeholder to
                // `@JvmInline` (single-field value classes). Mirror that so value-class tests reach the
                // compiler instead of failing to parse on the bare placeholder identifier.
                let src = krusty::conformance::prepare_test_source(&src);
                t_read.fetch_add(tr0.elapsed().as_nanos() as u64, Ordering::Relaxed);
                let __ret = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let applicable = if no_run {
                        frontend_applicable(&src, krusty::conformance::BACKENDS)
                    } else {
                        backend_applicable(&src, krusty::conformance::BACKENDS)
                    };
                    if !applicable {
                        return (file.clone(), TestResult::NotApplicable);
                    }
                    // In-process compilation. A `// WITH_STDLIB` test gets the kotlin-stdlib jar on krusty's
                    // classpath (so stdlib aliases/types resolve); others compile with no stdlib.
                    let stem = file
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("File")
                        .to_string();
                    // Directive-exact compile classpath (WITH_STDLIB/WITH_REFLECT/STDLIB_JDK8/WITH_COROUTINES).
                    mark_box_case_phase(&active, tid, file, "classpath");
                    let tj0 = std::time::Instant::now();
                    let compile_cp = common::classpath_jars_for(&src);
                    t_cpjars.fetch_add(tj0.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    let t0 = std::time::Instant::now();
                    mark_box_case_phase(&active, tid, file, "compile");
                    // A `// FILE:` multi-file test, OR a `// WITH_COROUTINES` test (which needs the
                    // generated `helpers` source compiled alongside it), goes through the multi-block path.
                    let compiled = if src.contains("// MODULE:") {
                        compile_module_test(&src, &compile_cp, jdk_modules.as_deref(), &|phase| {
                            mark_box_case_phase(&active, tid, file, phase)
                        })
                    } else if src.contains("// FILE:") || src.contains("// WITH_COROUTINES") {
                        compile_multifile(&src, &stem, &compile_cp, jdk_modules.as_deref())
                    } else {
                        compile_source(&src, &stem, &compile_cp, jdk_modules.as_deref(), &|phase| {
                            mark_box_case_phase(&active, tid, file, phase)
                        })
                    };
                    let classes = match compiled {
                        Some(c) => c,
                        None => {
                            return (
                                file.clone(),
                                TestResult::Fail("compiler rejected the corpus case".to_string()),
                            )
                        }
                    };
                    t_compile.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    mark_box_case_phase(&active, tid, file, "post-compile");
                    if byte_diff_on {
                        let outcome = byte_diff_file(&src, &stem, &compile_cp, &classes);
                        byte_diffs.lock().unwrap().push((file.clone(), outcome));
                    }
                    let box_class = match find_box_class(&classes) {
                        Some(c) => c,
                        None => {
                            return (
                                file.clone(),
                                TestResult::Fail(
                                    "emitted classes contain no `box()` entry point".to_string(),
                                ),
                            )
                        }
                    };

                    // KRUSTY_NO_RUN: compile + lower only (no JVM execution) — for profiling the
                    // front-end/codegen cost in isolation. A lowered file counts as Pass.
                    if no_run {
                        return (file.clone(), TestResult::Pass);
                    }

                    // Execute in the per-thread persistent JVM.
                    let mut guard = runners[tid].lock().unwrap();
                    if guard.is_none() {
                        *guard = Some(BoxRunner::new(&java, &runner_cp_str, &stdlib));
                    }
                    let runner = guard.as_mut().unwrap();
                    let t1 = std::time::Instant::now();
                    mark_box_case_phase(&active, tid, file, "jvm");
                    let result = match runner.run(&classes, &box_class) {
                        Some(r) => r,
                        None => {
                            // BoxRunner died (JVM crash/OOM); restart it for the next test.
                            *guard = None;
                            "ERROR:BoxRunnerCrash".to_string()
                        }
                    };
                    t_jvm.fetch_add(t1.elapsed().as_nanos() as u64, Ordering::Relaxed);

                    if result == "OK" {
                        (file.clone(), TestResult::Pass)
                    } else {
                        (file.clone(), TestResult::Fail(result))
                    }
                }))
                .unwrap_or_else(|panic| {
                    let message = panic
                        .downcast_ref::<String>()
                        .map(String::as_str)
                        .or_else(|| panic.downcast_ref::<&str>().copied())
                        .unwrap_or("non-string panic payload");
                    (
                        file.clone(),
                        TestResult::Fail(format!("compiler panic: {message}")),
                    )
                });
                t_closure.fetch_add(tc0.elapsed().as_nanos() as u64, Ordering::Relaxed);
                if let TestResult::Fail(why) = &__ret.1 {
                    let summary = why.lines().next().unwrap_or(why).to_string();
                    recent_failures
                        .lock()
                        .unwrap()
                        .push((file.clone(), summary));
                }
                *active[tid].lock().unwrap() = None;
                completed.fetch_add(1, Ordering::Relaxed);
                __ret
            })
            .collect()
    });
    progress_done.store(true, Ordering::Relaxed);
    progress_thread.thread().unpark();
    progress_thread.join().unwrap();

    // Peak-memory snapshot (KRUSTY_MEM_REPORT=1): process RSS after all compiles, the point where the
    // classpath caches are warmest. Confirms whether the per-thread LRU caps bound RSS. For a per-cache /
    // per-allocation-site breakdown, run this binary under `dhat`/`heaptrack` or call `Classpath::
    // cache_report()` from a focused profiling test.
    if env("KRUSTY_MEM_REPORT").is_some() {
        match peak_process_rss_kb() {
            Some(rss_kb) => eprintln!(
                "mem: peak process RSS = {:.1} MiB ({} threads)",
                rss_kb as f64 / 1024.0,
                n_threads,
            ),
            None => eprintln!("mem: peak process RSS unavailable ({} threads)", n_threads),
        }
    }
    // Cache hit-rate summary: whole-process efficiency of every classpath cache, for sizing the caps.
    // Emitted through the `cache` trace category (build `--features trace`, run `KRUSTY_TRACE=cache`);
    // a no-op otherwise, so it never touches the differential harness's stdout/stderr on a normal run.
    krusty::jvm::classpath::trace_cache_stats();

    // Emit the flamegraph (if profiling was on) before computing summaries.
    if let Some(g) = flame_guard {
        write_flamegraph(&g);
    }

    let total_ms = t_total_start.elapsed().as_millis();
    let compile_ms = t_compile.load(Ordering::Relaxed) / 1_000_000;
    let jvm_ms = t_jvm.load(Ordering::Relaxed) / 1_000_000;
    let lex_ms = T_LEX.load(Ordering::Relaxed) / 1_000_000;
    let parse_ms = T_PARSE.load(Ordering::Relaxed) / 1_000_000;
    let sigs_ms = T_SIGS.load(Ordering::Relaxed) / 1_000_000;
    let check_ms = T_CHECK.load(Ordering::Relaxed) / 1_000_000;
    let emit_ms = T_EMIT.load(Ordering::Relaxed) / 1_000_000;
    let closure_ms = t_closure.load(Ordering::Relaxed) / 1_000_000;
    let read_ms = t_read.load(Ordering::Relaxed) / 1_000_000;
    let cpjars_ms = t_cpjars.load(Ordering::Relaxed) / 1_000_000;
    eprintln!("timing (wall={total_ms}ms, thread-sum): closure={closure_ms}ms [read={read_ms}ms cpjars={cpjars_ms}ms compile={compile_ms}ms (lex={lex_ms} parse={parse_ms} sigs={sigs_ms} check={check_ms} emit={emit_ms}) jvm={jvm_ms}ms]");

    let _ = fs::remove_dir_all(&work);

    let mut compiled = 0usize;
    let mut passed = 0usize;
    let mut not_applicable = 0usize;
    let mut failures: Vec<String> = Vec::new();

    // KRUSTY_BOX_LIST=<path>: write one `PASS|FAIL <file>` line per corpus file, sorted.
    let mut listing: Vec<String> = Vec::new();
    for (file, r) in &results {
        match r {
            TestResult::NotApplicable => not_applicable += 1,
            TestResult::Pass => {
                compiled += 1;
                passed += 1;
            }
            TestResult::Fail(why) => {
                compiled += 1;
                failures.push(format!("{}: {why}", file.display()));
            }
        }
        if env("KRUSTY_BOX_LIST").is_some() {
            let tag = match r {
                TestResult::NotApplicable => "NOT-APPLICABLE",
                TestResult::Pass => "PASS",
                TestResult::Fail(_) => "FAIL",
            };
            listing.push(format!("{tag} {}", file.display()));
        }
    }
    if let Some(path) = env("KRUSTY_BOX_LIST") {
        listing.sort();
        let _ = fs::write(&path, listing.join("\n"));
    }

    // Performance + coverage trend log: append one CSV row per run so trends are visible over time.
    // Under target/ (untracked); inspect with `column -ts, target/ir_conformance_trend.csv`.
    {
        use std::io::Write;
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/ir_conformance_trend.csv");
        let new = !path.exists();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            if new {
                let _ = writeln!(f, "epoch,scanned,compiled,passed,failed,wall_ms,compile_ms,lex_ms,parse_ms,sigs_ms,check_ms,emit_ms,jvm_ms");
            }
            let _ = writeln!(f, "{epoch},{},{compiled},{passed},{},{total_ms},{compile_ms},{lex_ms},{parse_ms},{sigs_ms},{check_ms},{emit_ms},{jvm_ms}", files.len(), failures.len());
        }
    }

    eprintln!("\n=== Kotlin codegen/box conformance ===");
    eprintln!(
        "cases: {}  | applicable: {}  | krusty-compiled: {compiled}  | box()=OK: {passed}  | FAIL: {}  | not-applicable: {not_applicable}",
        files.len(),
        files.len() - not_applicable,
        failures.len()
    );
    if byte_diff_on {
        let mut diffs = byte_diffs.into_inner().unwrap();
        diffs.sort_by(|a, b| a.0.cmp(&b.0));
        let (mut identical, mut divergent, mut ref_fail, mut not_diffed) = (0usize, 0, 0, 0);
        let report_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/byte_diff_report.txt");
        let mut report = String::new();
        for (file, d) in &diffs {
            let (tag, why) = match d {
                ByteDiff::Identical => {
                    identical += 1;
                    ("IDENTICAL", String::new())
                }
                ByteDiff::Divergent(w) => {
                    divergent += 1;
                    ("DIVERGENT", w.clone())
                }
                ByteDiff::RefFail(w) => {
                    ref_fail += 1;
                    ("REF-FAIL", w.clone())
                }
                ByteDiff::NotDiffed(w) => {
                    not_diffed += 1;
                    ("NOT-DIFFED", w.to_string())
                }
            };
            report.push_str(&format!("{tag}\t{}\t{why}\n", file.display()));
        }
        let _ = fs::write(&report_path, &report);
        eprintln!(
            "byte-diff: identical {identical} | divergent {divergent} | ref-fail {ref_fail} | not-diffed {not_diffed}  (report: {})",
            report_path.display()
        );
    }
    let fail_cap = env("KRUSTY_FAIL_CAP")
        .and_then(|v| v.parse().ok())
        .unwrap_or(25usize);
    for f in failures.iter().take(fail_cap) {
        eprintln!("  FAIL {f}");
    }
    if env("KRUSTY_BOX_LIST").is_some() {
        for (file, r) in &results {
            let tag = match r {
                TestResult::NotApplicable => "NOT-APPLICABLE",
                TestResult::Pass => "PASS",
                TestResult::Fail(_) => "FAIL",
            };
            eprintln!("  {tag} {}", file.display());
        }
    }
    if let Some(path) = env("KRUSTY_CONFORMANCE_REPORT") {
        fs::write(&path, conformance_report(files.len(), passed))
            .unwrap_or_else(|err| panic!("failed to write conformance report: {err}"));
    }
    let applicable = files.len() - not_applicable;
    assert!(
        passed * 100 >= applicable * 55,
        "box conformance is below 55% of backend-applicable cases: {passed}/{applicable} passed; {} failed; {not_applicable} not applicable",
        failures.len()
    );
    assert!(
        passed > 0,
        "no box() cases ran — check Kotlin box corpus discovery / JDK"
    );
}

enum TestResult {
    NotApplicable,
    Pass,
    Fail(String),
}

// ===================== Byte-identity differential mode =====================
//
// `KRUSTY_BYTE_DIFF=1`: every corpus file krusty compiles is ALSO compiled with the reference
// kotlinc (persistent in-process server, content-keyed on-disk cache under
// `target/cache/ref-classes/`) and the two class sets are compared BYTE-FOR-BYTE. Metric line:
// `byte-diff: identical I | divergent D | ref-fail R | not-diffed N`; per-file detail lands in
// `target/byte_diff_report.txt`. Multi-module (`// MODULE:`) and mixed-Java tests are not diffed
// yet (their reference orchestration — module chaining, javac interleaving — isn't mirrored).

enum ByteDiff {
    Identical,
    Divergent(String),
    RefFail(String),
    NotDiffed(&'static str),
}

fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

/// Byte-compare krusty's emitted class set against the reference compiler's: the same class-name
/// set, every class byte-equal. `Err` carries the FIRST difference (one per file keeps the report
/// scannable; the next run surfaces the next difference).
fn compare_class_sets(
    krusty: &[(String, Vec<u8>)],
    reference: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    let mut k: std::collections::BTreeMap<&str, &[u8]> = krusty
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    for (name, rbytes) in reference {
        let Some(kb) = k.remove(name.as_str()) else {
            return Err(format!("missing class {name}"));
        };
        if kb != rbytes.as_slice() {
            let off = kb
                .iter()
                .zip(rbytes.iter())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| kb.len().min(rbytes.len()));
            return Err(format!(
                "class {name}: bytes differ at offset {off} (krusty {} B, kotlinc {} B)",
                kb.len(),
                rbytes.len()
            ));
        }
    }
    if let Some((extra, _)) = k.iter().next() {
        return Err(format!("extra class {extra}"));
    }
    Ok(())
}

/// `// LANGUAGE: +X -Y` directives → the `-XXLanguage:` flags the reference test runner passes.
fn language_directive_args(src: &str) -> Vec<String> {
    src.lines()
        .filter_map(|l| l.trim().strip_prefix("// LANGUAGE:"))
        .flat_map(|rest| rest.split_whitespace())
        .map(|tok| format!("-XXLanguage:{tok}"))
        .collect()
}

/// Bump when the cache layout / comparison semantics change — invalidates every cached entry.
const REF_CACHE_SALT: &str = "ref-classes-v1";

/// Read every `.class` under `dir` into an internal-name → bytes map. `Err` on any read failure
/// (a concurrently rewritten cache must re-grade as RefFail, not compare against empty bytes).
fn read_class_tree(dir: &Path) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = fs::read_dir(&d).map_err(|e| format!("read dir {}: {e}", d.display()))?;
        for e in rd {
            let e = e.map_err(|e| format!("read dir entry in {}: {e}", d.display()))?;
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "class") {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap()
                    .with_extension("")
                    .to_string_lossy()
                    .into_owned();
                let bytes = fs::read(&p).map_err(|e| format!("cache read {rel}: {e}"))?;
                out.insert(rel, bytes);
            }
        }
    }
    Ok(out)
}

/// Compile `src` with the reference kotlinc into a class-name → bytes map. Results (success AND
/// definitive compile-failure) are cached on disk, keyed by source content, file stem + block
/// names (the file name decides kotlinc's facade class name), classpath jar paths, the injected
/// helpers text, the reference dist identity, and a schema salt — so re-runs pay only for files
/// whose inputs changed. Transient failures (driver crash, work-dir clobber) are NOT cached.
/// `Err` = reference compile failed / unavailable.
///
/// NOTE: only `.class` artifacts are compared; kotlinc's `META-INF/<m>.kotlin_module` is a known
/// not-yet-compared artifact (the harness compile path doesn't produce krusty's module file).
fn reference_compile(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    let mut key_material: Vec<u8> = Vec::new();
    let push = |k: &mut Vec<u8>, part: &[u8]| {
        k.extend_from_slice(&(part.len() as u64).to_le_bytes());
        k.extend_from_slice(part);
    };
    push(&mut key_material, REF_CACHE_SALT.as_bytes());
    push(&mut key_material, src.as_bytes());
    push(&mut key_material, stem.as_bytes());
    push(&mut key_material, COROUTINE_HELPERS.as_bytes());
    for j in cp_jars {
        push(&mut key_material, j.to_string_lossy().as_bytes());
    }
    if let Some(jar) = common::kotlin_compiler_jar() {
        push(&mut key_material, jar.to_string_lossy().as_bytes());
        let len = fs::metadata(&jar).map(|m| m.len()).unwrap_or(0);
        push(&mut key_material, &len.to_le_bytes());
    }
    let key = fnv64(&key_material);
    let cache =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/cache/ref-classes/{key:016x}"));
    if cache.join("FAILED").is_file() {
        let why = fs::read_to_string(cache.join("FAILED")).unwrap_or_default();
        return Err(why.lines().next().unwrap_or("?").to_string());
    }
    if cache.join("OK").is_file() {
        return read_class_tree(&cache);
    }

    let (mut blocks, java_blocks) = krusty::conformance::split_files(src);
    if !java_blocks.is_empty() {
        return Err("mixed java (not diffed)".to_string());
    }
    if blocks.is_empty() {
        blocks.push((stem.to_string(), src.to_string()));
    }
    if src.contains("// WITH_COROUTINES") {
        blocks.push(("CoroutineUtil".to_string(), COROUTINE_HELPERS.to_string()));
    }
    // Unique work dir per COMPILE (not per key): two rayon threads can race the same key — a
    // shared dir would let one thread's cleanup delete the other's sources mid-compile. Every
    // return path below goes through the closure so the work dir is always removed (leaked
    // `/tmp/krusty_refc_*` dirs fill the shared disk).
    static WORK_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = WORK_SEQ.fetch_add(1, Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!(
        "krusty_refc_{key:016x}_{}_{seq}",
        std::process::id()
    ));
    let result = reference_compile_in(&work, &blocks, src, cp_jars, &cache, key, seq);
    let _ = fs::remove_dir_all(&work);
    result
}

/// The work-dir-scoped body of [`reference_compile`] — the caller removes `work` unconditionally.
#[allow(clippy::too_many_arguments)]
fn reference_compile_in(
    work: &Path,
    blocks: &[(String, String)],
    src: &str,
    cp_jars: &[PathBuf],
    cache: &Path,
    key: u64,
    seq: u64,
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    let out_dir = work.join("out");
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let mut args: Vec<String> = vec!["-d".into(), out_dir.to_string_lossy().into_owned()];
    if !cp_jars.is_empty() {
        let joined = cp_jars
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":");
        args.push("-cp".into());
        args.push(joined);
    }
    args.extend(language_directive_args(src));
    for (i, (name, content)) in blocks.iter().enumerate() {
        // The FILE NAME decides kotlinc's file-facade class name (`arrayElement.kt` →
        // `ArrayElementKt`), so it must be exactly the block's declared LEAF name
        // (`split_files` already stripped `.kt`). Each block gets its own numbered subdir so
        // same-leaf blocks (`a/x.kt` + `b/x.kt`) can't overwrite each other.
        let leaf = name.rsplit('/').next().unwrap_or(name);
        let bdir = work.join(format!("{i}"));
        fs::create_dir_all(&bdir).map_err(|e| e.to_string())?;
        let path = bdir.join(format!("{leaf}.kt"));
        fs::write(&path, content).map_err(|e| e.to_string())?;
        args.push(path.to_string_lossy().into_owned());
    }
    let (code, err) =
        common::kotlinc_compile(&args).ok_or_else(|| "kotlinc unavailable".to_string())?;
    if code == 0 {
        let classes = read_class_tree(&out_dir)?;
        // Publish ATOMICALLY: write the tree + OK marker into a unique staging dir, then rename it
        // to the cache path. A concurrent publisher's rename simply loses (its staging dir is
        // discarded); an already-published cache is never deleted out from under a reader.
        let staging = cache.with_file_name(format!("{key:016x}.stage{}-{seq}", std::process::id()));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
        for (name, bytes) in &classes {
            let p = staging.join(format!("{name}.class"));
            if let Some(parent) = p.parent() {
                let _ = fs::create_dir_all(parent);
            }
            fs::write(&p, bytes).map_err(|e| e.to_string())?;
        }
        fs::write(staging.join("OK"), b"").map_err(|e| e.to_string())?;
        if fs::rename(&staging, cache).is_err() {
            let _ = fs::remove_dir_all(&staging); // another thread published first
        }
        Ok(classes)
    } else {
        // Cache the failure ONLY for a SOURCE-ANCHORED diagnostic (`….kt:<line>:<col>: error:` —
        // deterministic for this source). A locationless `error:` (out of disk, output-dir
        // trouble) or a driver crash (the server returns 2 for any Throwable, no `error:` line)
        // stays uncached so the next run retries instead of being poisoned.
        match err
            .lines()
            .find(|l| l.contains(": error:") && l.contains(".kt:"))
        {
            Some(first) => {
                let first = first.to_string();
                let _ = fs::create_dir_all(cache);
                let _ = fs::write(cache.join("FAILED"), &first);
                Err(first)
            }
            None => Err(format!(
                "transient: {}",
                err.lines()
                    .find(|l| l.contains("error"))
                    .or_else(|| err.lines().next())
                    .unwrap_or("?")
            )),
        }
    }
}

/// The full per-file byte-diff decision: gate un-mirrored shapes, reference-compile, compare.
fn byte_diff_file(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    classes: &[(String, Vec<u8>)],
) -> ByteDiff {
    if src.contains("// MODULE:") {
        return ByteDiff::NotDiffed("multi-module");
    }
    if !krusty::conformance::split_files(src).1.is_empty() {
        return ByteDiff::NotDiffed("mixed-java");
    }
    match reference_compile(src, stem, cp_jars) {
        Err(e) => ByteDiff::RefFail(e),
        Ok(ref_classes) => match compare_class_sets(classes, &ref_classes) {
            Ok(()) => ByteDiff::Identical,
            Err(why) => ByteDiff::Divergent(why),
        },
    }
}

#[test]
fn compare_class_sets_identical_and_divergent() {
    use std::collections::BTreeMap;
    let k = vec![("A".to_string(), vec![1u8, 2, 3])];
    let mut r = BTreeMap::new();
    r.insert("A".to_string(), vec![1u8, 2, 3]);
    assert!(compare_class_sets(&k, &r).is_ok());

    r.insert("A".to_string(), vec![1u8, 9, 3]);
    let e = compare_class_sets(&k, &r).unwrap_err();
    assert!(e.contains("offset 1"), "{e}");

    let mut r2 = BTreeMap::new();
    r2.insert("A".to_string(), vec![1u8, 2, 3]);
    r2.insert("B".to_string(), vec![4u8]);
    assert!(compare_class_sets(&k, &r2)
        .unwrap_err()
        .contains("missing class B"));

    let r3 = BTreeMap::new();
    assert!(compare_class_sets(&k, &r3)
        .unwrap_err()
        .contains("extra class A"));
}

#[test]
fn language_directive_args_parse() {
    let src = "// LANGUAGE: +ContextParameters -SomethingElse\nfun box() = \"OK\"\n";
    assert_eq!(
        language_directive_args(src),
        vec![
            "-XXLanguage:+ContextParameters",
            "-XXLanguage:-SomethingElse"
        ]
    );
}
