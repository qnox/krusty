//! Shared test helpers.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use krusty::jvm::classpath::Classpath;

/// Compile Kotlin `src` to `(internal_name, class_bytes)` pairs entirely in-process — the same pipeline
/// (`lex → parse → check → ir_lower → ir_emit`) the conformance harness uses, sharing the process-global
/// classpath caches (type/ext/jimage indexes) across every call. This is dramatically faster than
/// spawning the `krusty` binary once per snippet (each subprocess rebuilds those indexes from scratch).
/// `cp_jars` are the `-classpath` jars; `jdk_modules` is the JDK `lib/modules` jimage (the bootclasspath).
/// Returns `None` on any compile error (an unsupported feature), like the CLI's non-zero exit.
#[allow(dead_code)]
pub fn compile_in_process(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<Vec<(String, Vec<u8>)>> {
    use krusty::diag::DiagSink;
    use krusty::jvm::names::file_class_name;
    use krusty::resolve::{check_file, collect_signatures_with_cp};

    let mut diags = DiagSink::new();
    // Language features are taken from the source's `// LANGUAGE:` directives, exactly as the kotlinc
    // test infrastructure supplies them — so flag-gated syntax compiles iff the test enables it.
    let features = krusty::features::LangFeatures::from_source(src);
    let toks = krusty::lexer::lex(src, &mut diags);
    let files = vec![krusty::parser::parse_with_features(
        src, &toks, &mut diags, &features,
    )];
    if diags.has_errors() {
        return None;
    }
    let mut cp_paths: Vec<PathBuf> = cp_jars.to_vec();
    if let Some(p) = jdk_modules {
        cp_paths.push(p.to_path_buf());
    }
    // Reuse one `Classpath` per classpath set on this thread (warm caches across snippets).
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
    let mut ir = krusty::ir_lower::lower_file(file, &info, &syms)?;
    // Compiler-extension plugins (kotlinx.serialization) — run them here exactly as the real backend
    // does (jvm/backend.rs), between lowering and the value-class pass. A no-op without `@Serializable`,
    // so non-serialization snippets are unaffected; with it, `compile_in_process` matches the binary.
    krusty::plugins::run_enabled(&mut ir, file);
    if !krusty::jvm::value_classes::lower_value_classes(&mut ir) {
        return None; // value-class shape not lowered — skip, don't miscompile
    }
    // The CPS (suspend) transform — the real backend (jvm/backend.rs) runs it right after the value-class
    // pass. Without it here, a `suspend` snippet compiled in-process would diverge from the gate.
    if !krusty::jvm::suspend::lower_suspend(&mut ir, &facade) {
        return None; // suspend shape not lowered — skip, don't miscompile
    }
    let outputs = krusty::jvm::ir_emit::emit_all(&ir, &facade, &*cp, None)?;
    if outputs.is_empty() {
        None
    } else {
        Some(outputs)
    }
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
    use krusty::resolve::{check_file, collect_signatures_with_cp};

    let mut diags = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(src);
    let toks = krusty::lexer::lex(src, &mut diags);
    let files = vec![krusty::parser::parse_with_features(
        src, &toks, &mut diags, &features,
    )];
    if diags.has_errors() {
        return None;
    }
    let mut cp_paths: Vec<PathBuf> = cp_jars.to_vec();
    if let Some(p) = jdk_modules {
        cp_paths.push(p.to_path_buf());
    }
    let cp = std::rc::Rc::new(Classpath::new(cp_paths));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let file = &files[0];
    let info = check_file(file, &mut syms, &mut diags);
    if diags.has_errors() {
        return None;
    }
    krusty::ir_lower::lower_file(file, &info, &syms)
}

/// Run a JavaScript source string on Node and return its stdout (trimmed), or `None` if `node` is
/// not on `PATH` (caller skips, exactly like a missing JVM). Used by the `js` backend e2e tests.
#[allow(dead_code)]
pub fn run_js(js: &str) -> Option<String> {
    let node = which_node()?;
    let dir = std::env::temp_dir().join(format!("krusty_js_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("m_{:x}.mjs", hash_str(js)));
    std::fs::write(&path, js).ok()?;
    // Bound wall time via `timeout` so a miscompiled loop can't hang the suite forever (exit 124).
    let out = Command::new("timeout")
        .arg("15s")
        .arg(&node)
        .arg(&path)
        .output()
        .ok()?;
    let _ = std::fs::remove_file(&path);
    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let tag = if code == 124 { "TIMEOUT" } else { "ERROR" };
        return Some(format!(
            "{tag}:{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn which_node() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("KRUSTY_NODE") {
        if !p.is_empty() && Path::new(&p).exists() {
            return Some(PathBuf::from(p));
        }
    }
    for dir in std::env::var("PATH").ok()?.split(':') {
        let cand = Path::new(dir).join("node");
        if cand.exists() {
            return Some(cand);
        }
    }
    None
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
    classpath_jars_for, dist_jar, ensure_maven, find_jar, jdk_modules, kotlin_test_jar,
    kotlin_version, kotlinc_lib_dir, stdlib_classpath, stdlib_jar,
};

/// Whether a box-test directive (`// NAME` …) is present. Single source of truth in the lib
/// (`krusty::conformance`), shared with the gate + survey so directive parsing never drifts.
#[allow(dead_code)]
pub fn directive(src: &str, name: &str) -> bool {
    krusty::conformance::directive(src, name)
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
                        return "ERROR:" + cause.getClass().getSimpleName() + ":" + cause.getMessage();
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
pub fn java_home() -> Option<String> {
    std::env::var("KRUSTY_REF_JAVA_HOME")
        .or_else(|_| std::env::var("JAVA_HOME"))
        .ok()
        .filter(|v| !v.is_empty())
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
    _child: Child,
    stdin: Mutex<ChildStdin>,
    waiters: Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>>,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
}

impl BoxRunner {
    fn new(java: &str, cp: &str) -> Option<Self> {
        let mut child = Command::new(java)
            .args(["-cp", cp, "BoxRunner"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
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
            _child: child,
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

type RunnerPool = Mutex<HashMap<String, Arc<BoxRunner>>>;

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
    static POOL: OnceLock<RunnerPool> = OnceLock::new();
    let java_home = java_home()?;
    let java = format!("{java_home}/bin/java");
    if !Path::new(&java).exists() {
        return None;
    }
    let runner_dir = setup_runner(&java_home)?;
    let mut cp = runner_dir.to_string_lossy().into_owned();
    for j in cp_jars {
        cp.push(':');
        cp.push_str(&j.to_string_lossy());
    }
    let pool = POOL.get_or_init(|| Mutex::new(HashMap::new()));

    // Fetch (or spin up) the runner for this classpath under the pool lock, then release the lock so
    // the actual round-trip runs concurrently with other threads' calls.
    let get_runner = || -> Option<Arc<BoxRunner>> {
        let mut map = pool.lock().unwrap();
        if map.get(&cp).is_none_or(|r| !r.alive.load(Ordering::SeqCst)) {
            map.insert(cp.clone(), Arc::new(BoxRunner::new(&java, &cp)?));
        }
        map.get(&cp).cloned()
    };

    let runner = get_runner()?;
    match runner.try_run(classes, box_class) {
        Ok(s) => Some(s),
        Err(_) => {
            // The JVM died or timed out. Replace the dead runner (if another thread hasn't already)
            // and retry once on a fresh one.
            {
                let mut map = pool.lock().unwrap();
                if map.get(&cp).is_some_and(|r| Arc::ptr_eq(r, &runner)) {
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

/// Compile Kotlin `lib_src` with the REAL kotlinc into a fresh classpath dir (tagged by `tag` +
/// process id), returning the output dir for a `-classpath`. `None` (→ skip the test) when the kotlinc
/// toolchain / stdlib isn't provisioned. The single shared "build a dependency jar" helper — classpath
/// e2e tests use this instead of each re-implementing the kotlinc invocation.
#[allow(dead_code)]
pub fn compile_lib(tag: &str, lib_src: &str) -> Option<PathBuf> {
    let stdlib = stdlib_jar()?;
    let work = std::env::temp_dir().join(format!("krusty_lib_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let out = work.join("libout");
    std::fs::create_dir_all(&out).ok()?;
    let lib_kt = work.join("Lib.kt");
    std::fs::write(&lib_kt, lib_src).ok()?;
    let args = vec![
        "-d".into(),
        out.to_string_lossy().into_owned(),
        "-cp".into(),
        stdlib.to_string_lossy().into_owned(),
        lib_kt.to_string_lossy().into_owned(),
    ];
    match kotlinc_compile(&args) {
        Some((0, _)) => Some(out),
        Some((code, err)) => panic!("kotlinc(lib) failed ({code}): {err}"),
        None => None,
    }
}

/// Compile `main` against a kotlinc-built `lib_src` (via [`compile_lib`]) and run its `box()` on the
/// persistent JVM. `None` (→ skip) when the toolchain is unavailable. stdlib + JDK modules are on both
/// the compile and run classpath.
#[allow(dead_code)]
pub fn run_box_against(tag: &str, lib_src: &str, main: &str) -> Option<String> {
    let libout = compile_lib(tag, lib_src)?;
    let stdlib = stdlib_jar()?;
    compile_and_run_box(main, "Main", &[libout, stdlib], jdk_modules().as_deref())
}

/// Compile `main` against a kotlinc-built `lib_src` up to the CHECKER only (no lowering/emit), returning
/// the diagnostic messages (empty = clean). For asserting the RESOLUTION of a shape whose end-to-end
/// lowering is an orthogonal, not-yet-implemented feature. `None` (→ skip) when the toolchain is absent.
#[allow(dead_code)]
pub fn checker_diags_against(tag: &str, lib_src: &str, main: &str) -> Option<Vec<String>> {
    use krusty::diag::DiagSink;
    use krusty::resolve::{check_file, collect_signatures_with_cp};
    let libout = compile_lib(tag, lib_src)?;
    let stdlib = stdlib_jar()?;
    let mut cp_paths = vec![libout, stdlib];
    if let Some(jdk) = jdk_modules() {
        cp_paths.push(jdk);
    }
    let mut diags = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(main);
    let toks = krusty::lexer::lex(main, &mut diags);
    let files = vec![krusty::parser::parse_with_features(
        main, &toks, &mut diags, &features,
    )];
    let cp = std::rc::Rc::new(Classpath::new(cp_paths));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    let _ = check_file(&files[0], &mut syms, &mut diags);
    Some(diags.diags.iter().map(|m| m.msg.clone()).collect())
}

/// The provisioned Kotlin codegen/box corpus directory (`KRUSTY_KOTLIN_BOX_DIR`), if present — the
/// SAME corpus the differential conformance gate runs over. Lets an e2e pin a SPECIFIC real corpus
/// case as a named regression test (instead of a hand-written snippet that may hit a lowering edge the
/// corpus case doesn't). `None` when the corpus isn't provisioned, so the test skips rather than fails.
#[allow(dead_code)]
pub fn box_corpus_dir() -> Option<PathBuf> {
    let d = std::env::var("KRUSTY_KOTLIN_BOX_DIR")
        .ok()
        .filter(|s| !s.is_empty())?;
    let p = PathBuf::from(d);
    p.is_dir().then_some(p)
}

/// Whether both the JVM toolchain AND the box corpus are provisioned (an e2e that runs a corpus case
/// needs both). `false` ⇒ the test should skip.
#[allow(dead_code)]
pub fn corpus_ready() -> bool {
    java_home().is_some() && stdlib_jar().is_some() && box_corpus_dir().is_some()
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
    let src = std::fs::read_to_string(box_corpus_dir()?.join(rel))
        .ok()?
        .replace("OPTIONAL_JVM_INLINE_ANNOTATION", "@JvmInline");
    // Multi-file / multi-module cases need the gate's `// FILE:`/`// MODULE:` splitting — skip here
    // rather than miscompile all blocks as one source (enforce the contract, don't rely on luck).
    if src.contains("// FILE:") || src.contains("// MODULE:") {
        return None;
    }
    let jh = java_home()?;
    let jdk = PathBuf::from(format!("{jh}/lib/modules"));
    let cp = classpath_jars_for(&src);
    let classes = compile_in_process(&src, "P", &cp, Some(&jdk))?;
    let box_class = find_box_class(&classes)?;
    run_box(&classes, &box_class, &cp)
}

// --- Persistent kotlinc compiler server -----------------------------------
//
// The reference `kotlinc` is a JVM program; spawning its CLI per test pays a ~2-4s JVM + compiler
// cold start each time (the dominant cost of the differential e2e). Instead run ONE persistent JVM
// that invokes the compiler class (`K2JVMCompiler.exec`, which returns an `ExitCode` without
// `System.exit`) in-process per request — the same thing the Kotlin compile daemon does. Class state
// from one compile doesn't leak destructively into the next (exec creates+disposes its own
// environment per call).

const KOTLINC_SERVER_SRC: &str = r#"
import java.io.*;
import org.jetbrains.kotlin.cli.jvm.K2JVMCompiler;
import org.jetbrains.kotlin.cli.common.ExitCode;

public class KotlincServer {
    public static void main(String[] a) throws Exception {
        DataInputStream din = new DataInputStream(new BufferedInputStream(System.in, 65536));
        DataOutputStream dout = new DataOutputStream(new BufferedOutputStream(System.out, 4096));
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
                ExitCode code = new K2JVMCompiler().exec(err, args);
                codeNum = code.getCode();
            } catch (Throwable t) {
                t.printStackTrace(err);
                codeNum = 2;
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
/// `K2JVMCompiler`. `None` when no `KRUSTY_KOTLINC` dist is available.
#[allow(dead_code)]
pub fn kotlin_compiler_jar() -> Option<PathBuf> {
    let p = kotlinc_lib_dir()?.join("kotlin-compiler.jar");
    p.is_file().then_some(p)
}

/// Compile `KotlincServer.java` once (against the compiler jar) into a stable cache dir; return it.
fn setup_kotlinc_server(java_home: &str, compiler_jar: &Path) -> Option<PathBuf> {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in KOTLINC_SERVER_SRC.bytes() {
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
        .args(["-cp", &compiler_jar.to_string_lossy(), "-d"])
        .arg(&dir)
        .arg(&src_path)
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!(
            "KotlincServer javac failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return None;
    }
    Some(dir)
}

/// A persistent JVM running `KotlincServer`, fed compiler arg-lists over a pipe.
struct KotlincServer {
    _child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl KotlincServer {
    fn new(java: &str, cp: &str) -> Option<Self> {
        let mut child = Command::new(java)
            // Test compiles aren't perf-critical; favour fast startup over peak codegen throughput
            // (cap JIT at C1, serial GC) so the one-time JVM+compiler warmup is small.
            .args([
                "-XX:TieredStopAtLevel=1",
                "-XX:+UseSerialGC",
                "-cp",
                cp,
                "KotlincServer",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
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
pub fn kotlinc_compile(args: &[String]) -> Option<(i32, String)> {
    static POOL: OnceLock<Mutex<HashMap<String, Mutex<KotlincServer>>>> = OnceLock::new();
    let java_home = java_home()?;
    let java = format!("{java_home}/bin/java");
    if !Path::new(&java).exists() {
        return None;
    }
    let compiler_jar = kotlin_compiler_jar()?;
    let server_dir = setup_kotlinc_server(&java_home, &compiler_jar)?;
    let cp = format!(
        "{}:{}",
        server_dir.to_string_lossy(),
        compiler_jar.to_string_lossy()
    );
    let pool = POOL.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let mut map = pool.lock().unwrap();
        if !map.contains_key(&cp) {
            map.insert(cp.clone(), Mutex::new(KotlincServer::new(&java, &cp)?));
        }
    }
    let map = pool.lock().unwrap();
    let server_mx = map.get(&cp).unwrap();
    let mut server = server_mx.lock().unwrap();
    match server.try_compile(args) {
        Ok(r) => Some(r),
        Err(_) => {
            // Server died — restart once and retry.
            *server = KotlincServer::new(&java, &cp)?;
            server.try_compile(args).ok()
        }
    }
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

public class JavaRunner {
    public static void main(String[] a) throws Exception {
        DataInputStream din = new DataInputStream(new BufferedInputStream(System.in, 65536));
        DataOutputStream dout = new DataOutputStream(new BufferedOutputStream(System.out, 4096));
        PrintStream realOut = System.out;
        while (true) {
            String driver, cp, outdir, mainClass;
            try { driver = readStr(din); } catch (EOFException e) { break; }
            cp = readStr(din); outdir = readStr(din); mainClass = readStr(din);
            String result;
            try {
                ByteArrayOutputStream jerr = new ByteArrayOutputStream();
                JavaCompiler jc = ToolProvider.getSystemJavaCompiler();
                int rc = jc.run(null, null, new PrintStream(jerr, true, "UTF-8"),
                        "-cp", cp, "-d", outdir, driver);
                if (rc != 0) {
                    result = "ERROR:javac:" + jerr.toString("UTF-8");
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
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in JAVA_RUNNER_SRC.bytes() {
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
        let mut child = Command::new(java)
            .args([
                "-Xverify:all",
                "-cp",
                &runner_dir.to_string_lossy(),
                "JavaRunner",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
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
    ) -> std::io::Result<String> {
        self.write_str(driver)?;
        self.write_str(cp)?;
        self.write_str(outdir)?;
        self.write_str(main_class)?;
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
    static POOL: OnceLock<Mutex<JavaRunner>> = OnceLock::new();
    let java_home = java_home()?;
    let java = format!("{java_home}/bin/java");
    if !Path::new(&java).exists() {
        return None;
    }
    let runner_dir = setup_java_runner(&java_home)?;
    let mx = POOL.get_or_init(|| Mutex::new(JavaRunner::new(&java, &runner_dir).unwrap()));
    let mut runner = mx.lock().unwrap();
    match runner.try_run(driver_path, cp, outdir, main_class) {
        Ok(s) => Some(s),
        Err(_) => {
            *runner = JavaRunner::new(&java, &runner_dir)?;
            runner.try_run(driver_path, cp, outdir, main_class).ok()
        }
    }
}
