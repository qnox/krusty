//! Kotlin compiler conformance suite (`compiler/testData/codegen/box`).
//!
//! Each `fun box(): String` → `"OK"` test is:
//!   * **skip**  — krusty compile error (unsupported feature)
//!   * **pass**  — compiles and box() returns "OK" on the JVM
//!   * **FAIL**  — compiled but produced wrong/invalid bytecode (a bug)
//!
//! Performance design:
//!   - In-process compilation (no krusty subprocess)
//!   - Rayon parallel compilation across all CPU cores
//!   - One persistent JVM runner per rayon thread (no per-test JVM restarts)
//!   - No javac: the runner loads bytes with a per-test ClassLoader + reflection
//!
//! Env vars:
//!   KRUSTY_KOTLIN_BOX_DIR   path to compiler/testData/codegen/box
//!   KRUSTY_REF_JAVA_HOME / JAVA_HOME
//!   KRUSTY_BOX_LIMIT        cap on files scanned (default: all)
//! The kotlin-stdlib jar is located from local caches (`common::stdlib_jar`) and supplied via
//! `-classpath` only to `// WITH_STDLIB` tests, plus the JVM runner's runtime classpath.

use std::fs;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rayon::prelude::*;

use krusty::diag::DiagSink;
use krusty::ir_lower::lower_file;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::classreader::parse_class;
use krusty::jvm::ir_emit;
use krusty::jvm::names::file_class_name;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures_with_cp};

mod common;

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

/// Whether a box test applies to the given backend(s), per kotlinc's directives:
/// `// TARGET_BACKEND:` restricts the test to the listed backends (absent = all);
/// `// IGNORE_BACKEND[_K1|_K2]:` excludes the listed backends. `names` is the set of backend tokens
/// the current run identifies as (e.g. `["JVM", "JVM_IR"]` or `["JS", "JS_IR"]`).
pub fn backend_applicable(src: &str, names: &[&str]) -> bool {
    let mentions = |line: &str| line.split(',').any(|t| names.contains(&t.trim()));
    if let Some(l) = src.lines().find(|l| l.starts_with("// TARGET_BACKEND:")) {
        if !mentions(l.trim_start_matches("// TARGET_BACKEND:").trim()) {
            return false;
        }
    }
    for l in src.lines().filter(|l| {
        l.starts_with("// IGNORE_BACKEND:")
            || l.starts_with("// IGNORE_BACKEND_K1:")
            || l.starts_with("// IGNORE_BACKEND_K2:")
    }) {
        let rest = l.split_once(':').map(|x| x.1).unwrap_or("");
        if mentions(rest.trim()) {
            return false;
        }
    }
    true
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn collect_kt(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(dir) {
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                collect_kt(&p, out);
            } else if p.extension().is_some_and(|e| e == "kt") {
                out.push(p);
            }
        }
    }
}

// Sub-phase timers (ns, accumulated across all files and threads).
static T_LEX: AtomicU64 = AtomicU64::new(0);
static T_PARSE: AtomicU64 = AtomicU64::new(0);
static T_SIGS: AtomicU64 = AtomicU64::new(0);
static T_CHECK: AtomicU64 = AtomicU64::new(0);
static T_EMIT: AtomicU64 = AtomicU64::new(0);

/// Compile Kotlin source to a list of (class_internal_name, class_bytes) pairs.
/// Returns None if compilation fails (unsupported feature).
thread_local! {
    /// One `Classpath` per (rayon thread, classpath set), reused across every file that thread
    /// compiles — the real `kotlinc`/`main.rs` builds the classpath once per invocation too, so
    /// rebuilding (and re-indexing the stdlib jar) per file was pure harness overhead. `Classpath` is
    /// `!Sync` (RefCell caches), so the cache is thread-local rather than shared across workers.
    static CP_CACHE: std::cell::RefCell<std::collections::HashMap<Vec<std::path::PathBuf>, std::rc::Rc<Classpath>>>
        = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn compile_source(
    src: &str,
    stem: &str,
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<Vec<(String, Vec<u8>)>> {
    let mut diags = DiagSink::new();
    let t0 = std::time::Instant::now();
    let toks = lex(src, &mut diags);
    T_LEX.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    let t1 = std::time::Instant::now();
    let files = vec![parse(src, &toks, &mut diags)];
    T_PARSE.fetch_add(t1.elapsed().as_nanos() as u64, Ordering::Relaxed);
    if diags.has_errors() {
        return None;
    }
    let t2 = std::time::Instant::now();
    // The stdlib is on krusty's classpath only for `// WITH_STDLIB` tests — the caller passes the
    // located jar (or `None`), exactly as a drop-in `kotlinc` user supplies `-classpath`.
    // Explicit classpath: the kotlin-stdlib jar (for `// WITH_STDLIB`) plus the JDK `lib/modules`
    // jimage (the bootclasspath). The compiler never reads `JAVA_HOME` — the harness passes the
    // path, exactly as a `kotlinc -classpath` invocation would.
    let mut cp_paths: Vec<std::path::PathBuf> = cp_jars.to_vec();
    if let Some(p) = jdk_modules {
        cp_paths.push(p.to_path_buf());
    }
    // Reuse a thread-local `Classpath` for this classpath set (warm caches across files).
    let cp = CP_CACHE.with(|c| {
        c.borrow_mut()
            .entry(cp_paths.clone())
            .or_insert_with(|| std::rc::Rc::new(Classpath::new(cp_paths.clone())))
            .clone()
    });
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let syms = collect_signatures_with_cp(&files, platform, &mut diags);
    T_SIGS.fetch_add(t2.elapsed().as_nanos() as u64, Ordering::Relaxed);
    if diags.has_errors() {
        return None;
    }
    let file = &files[0];
    let t3 = std::time::Instant::now();
    let info = check_file(file, &syms, &mut diags);
    T_CHECK.fetch_add(t3.elapsed().as_nanos() as u64, Ordering::Relaxed);
    if diags.has_errors() {
        return None;
    }

    let facade_name = file_class_name(stem, file.package.as_deref());

    let t4 = std::time::Instant::now();
    // Lower the checked file to krusty-ir, then emit JVM bytecode (the sole codegen path).
    let ir = match lower_file(file, &info, &syms) {
        Some(ir) => ir,
        None => {
            T_EMIT.fetch_add(t4.elapsed().as_nanos() as u64, Ordering::Relaxed);
            return None;
        }
    };
    let outputs: Vec<(String, Vec<u8>)> = match ir_emit::emit_all(&ir, &facade_name, &*cp) {
        Some(o) => o,
        None => {
            T_EMIT.fetch_add(t4.elapsed().as_nanos() as u64, Ordering::Relaxed);
            return None;
        }
    };
    T_EMIT.fetch_add(t4.elapsed().as_nanos() as u64, Ordering::Relaxed);

    if outputs.is_empty() {
        return None;
    }
    Some(outputs)
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

#[test]
fn kotlin_codegen_box_conformance() {
    let box_dir = env("KRUSTY_KOTLIN_BOX_DIR").unwrap_or_else(|| {
        panic!(
            "KRUSTY_KOTLIN_BOX_DIR not set — the Kotlin codegen/box corpus is required for the \
             conformance gate, not optional. Run via `just test`/`just conformance` (provisions + \
             caches it per supported version) or set it manually. Refusing to skip.",
        )
    });
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
    let stdlib_jar = common::stdlib_jar();
    // Runtime classpath: every candidate stdlib-family jar (kotlin-stdlib, kotlin-test, reflect,
    // stdlib-jdk8, coroutines, annotations). The per-thread JVM has a fixed classpath, and extra
    // jars are harmless to tests that don't use them; the *compile* classpath stays directive-exact.
    let stdlib = {
        let mut paths: Vec<String> = Vec::new();
        for p in [
            stdlib_jar.clone(),
            common::kotlin_test_jar(),
            common::find_jar("kotlin-reflect-", &[]),
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
    let limit: usize = env("KRUSTY_BOX_LIMIT")
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);

    let mut files = Vec::new();
    collect_kt(Path::new(&box_dir), &mut files);
    // KRUSTY_BOX_LIMIT caps the run for fast dev rounds. Sample evenly across the *sorted* corpus
    // (a stride) rather than truncating to the first N — the first N are all `annotations/…`, which
    // would hide coverage in every other package. A full (unset) run keeps the whole corpus.
    if limit < files.len() {
        let stride = files.len() / limit;
        files = files.into_iter().step_by(stride.max(1)).collect();
    }

    let work = std::env::temp_dir().join(format!("krusty_box_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    // Compile BoxRunner.java once.
    let runner_cp = setup_runner(&java_home, &work);
    let runner_cp_str = runner_cp.to_str().unwrap().to_string();

    // Build a thread pool with a large stack (8 MiB) so deeply-nested source files don't
    // overflow the default 2 MiB Rayon stack during recursive descent parsing/checking.
    let pool = rayon::ThreadPoolBuilder::new()
        // Generous worker stacks: the recursive-descent parser/lowering uses far more stack per frame in
        // an unoptimized (`--profile gate`) build than in `--release`, so a deeply nested test would
        // overflow an 8 MB stack. 64 MB keeps the fast-iteration profile robust.
        .stack_size(64 * 1024 * 1024)
        .build()
        .unwrap();
    let n_threads = pool.current_num_threads();
    let runners: Vec<Mutex<Option<BoxRunner>>> = (0..n_threads).map(|_| Mutex::new(None)).collect();

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

    // Parallel phase: compile each test in-process, run in the per-thread JVM.
    let results: Vec<(PathBuf, TestResult)> = pool.install(|| {
        files
            .par_iter()
            .map(|file| {
                let tc0 = std::time::Instant::now();
                let tr0 = std::time::Instant::now();
                let src = fs::read_to_string(file).unwrap_or_default();
                t_read.fetch_add(tr0.elapsed().as_nanos() as u64, Ordering::Relaxed);
                let __ret = (|| {
                    // Skip multi-file, multi-module, or no-box tests.
                    if src.contains("// FILE:")
                        || src.contains("// MODULE:")
                        || !src.contains("fun box()")
                    {
                        return (file.clone(), TestResult::Skip);
                    }
                    // Skip tests that require invokedynamic lambdas or features not supported on JVM_IR K2.
                    if src.contains("// LAMBDAS: INDY") || src.contains("IGNORE_BACKEND_K2: JVM_IR")
                    {
                        return (file.clone(), TestResult::Skip);
                    }
                    // Respect the backend directives: a `// TARGET_BACKEND:` that excludes JVM, or an
                    // `// IGNORE_BACKEND[_K1/_K2]:` that names JVM/JVM_IR, means this test is not for us.
                    if !backend_applicable(&src, &["JVM", "JVM_IR"]) {
                        return (file.clone(), TestResult::Skip);
                    }
                    // Skip tests that rely on unsigned-integer-to-string conversion with unsigned semantics.
                    if src.contains("U.toString()") || src.contains("UL.toString()") {
                        return (file.clone(), TestResult::Skip);
                    }
                    // Skip tests that combine typealias-of-function-type with suspend conversion:
                    // krusty doesn't resolve typealiases, so the lambda arity is wrong.
                    if src.contains("typealias") && src.contains(": suspend (") {
                        return (file.clone(), TestResult::Skip);
                    }

                    // In-process compilation. A `// WITH_STDLIB` test gets the kotlin-stdlib jar on krusty's
                    // classpath (so stdlib aliases/types resolve); others compile with no stdlib.
                    let stem = file
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("File")
                        .to_string();
                    // Directive-exact compile classpath (WITH_STDLIB/WITH_REFLECT/STDLIB_JDK8/WITH_COROUTINES).
                    let tj0 = std::time::Instant::now();
                    let compile_cp = common::classpath_jars_for(&src);
                    t_cpjars.fetch_add(tj0.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    let t0 = std::time::Instant::now();
                    let classes =
                        match compile_source(&src, &stem, &compile_cp, jdk_modules.as_deref()) {
                            Some(c) => c,
                            None => return (file.clone(), TestResult::Skip),
                        };
                    t_compile.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    let box_class = match find_box_class(&classes) {
                        Some(c) => c,
                        None => return (file.clone(), TestResult::Skip),
                    };

                    // KRUSTY_NO_RUN: compile + lower only (no JVM execution) — for profiling the
                    // front-end/codegen cost in isolation. A lowered file counts as Pass.
                    if no_run {
                        return (file.clone(), TestResult::Pass);
                    }

                    // Execute in the per-thread persistent JVM.
                    let tid = rayon::current_thread_index().unwrap_or(0);
                    let mut guard = runners[tid].lock().unwrap();
                    if guard.is_none() {
                        *guard = Some(BoxRunner::new(&java, &runner_cp_str, &stdlib));
                    }
                    let runner = guard.as_mut().unwrap();
                    let t1 = std::time::Instant::now();
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
                })();
                t_closure.fetch_add(tc0.elapsed().as_nanos() as u64, Ordering::Relaxed);
                __ret
            })
            .collect()
    });

    // Emit the flamegraph (if profiling was on) before computing summaries.
    if let Some(g) = flame_guard {
        if let Ok(report) = g.report().build() {
            let path =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/flamegraph.svg");
            if let Ok(f) = std::fs::File::create(&path) {
                let _ = report.flamegraph(f);
                eprintln!("flamegraph written to {}", path.display());
            }
            // Terminal-readable hotspots: aggregate samples by leaf frame, print the top 25.
            let mut leaf: std::collections::HashMap<String, isize> =
                std::collections::HashMap::new();
            let mut total: isize = 0;
            for (frames, count) in &report.data {
                total += *count;
                if let Some(top) = frames.frames.first().and_then(|f| f.first()) {
                    *leaf.entry(top.name()).or_default() += *count;
                }
            }
            let mut v: Vec<_> = leaf.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            eprintln!("--- profiler: top self-frames ({total} samples) ---");
            for (name, c) in v.into_iter().take(25) {
                eprintln!("  {:>5.1}%  {name}", 100.0 * c as f64 / total.max(1) as f64);
            }
        }
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
    let mut skipped = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (file, r) in &results {
        match r {
            TestResult::Skip => skipped += 1,
            TestResult::Pass => {
                compiled += 1;
                passed += 1;
            }
            TestResult::Fail(why) => {
                compiled += 1;
                failures.push(format!("{}: {why}", file.display()));
            }
        }
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
        "scanned: {}  | krusty-compiled: {compiled}  | box()=OK: {passed}  | skipped(unsupported): {skipped}  | FAIL: {}",
        files.len(),
        failures.len()
    );
    for f in failures.iter().take(25) {
        eprintln!("  FAIL {f}");
    }
    assert!(
        failures.is_empty(),
        "{} box case(s) miscompiled (see above)",
        failures.len()
    );
    assert!(
        passed > 0,
        "no box() cases ran — check KRUSTY_KOTLIN_BOX_DIR / JDK"
    );
}

enum TestResult {
    Skip,
    Pass,
    Fail(String),
}
