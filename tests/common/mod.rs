//! Shared test helpers.

use std::collections::HashMap;
use std::io::Write as _;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};
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
    let syms = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let file = &files[0];
    let info = check_file(file, &syms, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let facade = file_class_name(stem, file.package.as_deref());
    let mut ir = krusty::ir_lower::lower_file(file, &info, &syms)?;
    if !krusty::jvm::value_classes::lower_value_classes(&mut ir) {
        return None; // value-class shape not lowered — skip, don't miscompile
    }
    let outputs = krusty::jvm::ir_emit::emit_all(&ir, &facade, &*cp, None)?;
    if outputs.is_empty() {
        None
    } else {
        Some(outputs)
    }
}

/// Locate a complete kotlin-stdlib jar from standard local caches, mirroring how a drop-in
/// `kotlinc` user supplies it via `-classpath`. "Complete" = the jar's `TypeAliasesKt` facades
/// yield type aliases when scanned (so `Exception` etc. resolve). Returns `None` if none is found,
/// in which case an exception-using test should skip (like a missing `JAVA_HOME`).
#[allow(dead_code)]
pub fn stdlib_jar() -> Option<PathBuf> {
    // Prefer the dist's own stdlib — the exact jar the reference compiler uses.
    if let Some(j) = dist_jar("kotlin-stdlib.jar") {
        let cp = Classpath::new(vec![j.clone()]);
        if !cp.scan_types().type_aliases.is_empty() {
            return Some(j);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let roots = [
        format!("{home}/.gradle"),
        format!("{home}/.m2/repository/org/jetbrains/kotlin"),
    ];
    let mut found = Vec::new();
    for r in &roots {
        collect_stdlib_jars(std::path::Path::new(r), &mut found, 0);
    }
    // Prefer a jar whose scan actually yields aliases (a real, non-stub stdlib).
    for jar in found {
        let cp = Classpath::new(vec![jar.clone()]);
        if !cp.scan_types().type_aliases.is_empty() {
            return Some(jar);
        }
    }
    None
}

/// A `Classpath` containing the located stdlib jar, or empty if none was found.
#[allow(dead_code)]
pub fn stdlib_classpath() -> Classpath {
    match stdlib_jar() {
        Some(j) => Classpath::new(vec![j]),
        None => Classpath::empty(),
    }
}

/// Whether a box-test directive (`// NAME` …) is present.
#[allow(dead_code)]
pub fn directive(src: &str, name: &str) -> bool {
    src.lines().any(|l| {
        let l = l.trim();
        l.starts_with("//")
            && l.trim_start_matches('/')
                .trim_start()
                .split([' ', ':'])
                .next()
                == Some(name)
    })
}

/// Locate the newest jar whose file name starts with `prefix` and ends with `.jar`, excluding
/// source/javadoc/other-target variants and any of `excludes` substrings.
#[allow(dead_code)]
pub fn find_jar(prefix: &str, excludes: &[&str]) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let roots = [
        format!("{home}/.gradle"),
        format!("{home}/.m2/repository/org/jetbrains"),
    ];
    let mut found = Vec::new();
    for r in &roots {
        collect_named_jars(std::path::Path::new(r), prefix, excludes, &mut found, 0);
    }
    // Prefer the shortest name (the plain `<prefix><version>.jar`, not `-junit`/`-jvm`/…).
    found.sort_by_key(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.len())
            .unwrap_or(usize::MAX)
    });
    found.into_iter().next()
}

fn collect_named_jars(
    dir: &std::path::Path,
    prefix: &str,
    excludes: &[&str],
    out: &mut Vec<PathBuf>,
    depth: usize,
) {
    if depth > 9 || out.len() > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_named_jars(&p, prefix, excludes, out, depth + 1);
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            let bad = ["sources", "javadoc", "-js", "wasm", "common", "metadata"];
            if name.starts_with(prefix)
                && name.ends_with(".jar")
                && !bad.iter().any(|b| name.contains(b))
                && !excludes.iter().any(|b| name.contains(b))
            {
                out.push(p);
            }
        }
    }
}

/// The `lib/` dir of the kotlinc dist we differential-test against (`KRUSTY_KOTLINC` points at its
/// `bin/kotlinc`). Its jars are the **exact** ones the reference compiler ships — the truest match,
/// so we prefer them over Maven/gradle copies. `None` when `KRUSTY_KOTLINC` is unset.
#[allow(dead_code)]
pub fn kotlinc_lib_dir() -> Option<PathBuf> {
    let kc = std::env::var("KRUSTY_KOTLINC").ok()?;
    let lib = PathBuf::from(kc).parent()?.parent()?.join("lib");
    lib.is_dir().then_some(lib)
}

/// A jar from the kotlinc dist `lib/` by exact (unversioned) file name, e.g. `kotlin-test.jar`.
#[allow(dead_code)]
pub fn dist_jar(name: &str) -> Option<PathBuf> {
    let p = kotlinc_lib_dir()?.join(name);
    p.is_file().then_some(p)
}

/// The Kotlin version to pin Maven fallbacks to — from the dist `build.txt` (e.g. `1.9.24-release-822`
/// → `1.9.24`) or a located versioned stdlib jar, defaulting to a known-good version.
#[allow(dead_code)]
pub fn kotlin_version() -> String {
    if let Some(lib) = kotlinc_lib_dir() {
        if let Ok(s) = std::fs::read_to_string(lib.parent().unwrap().join("build.txt")) {
            if let Some(v) = s.trim().split('-').next() {
                if !v.is_empty() {
                    return v.to_string();
                }
            }
        }
    }
    stdlib_jar()
        .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
        .and_then(|n| {
            n.strip_prefix("kotlin-stdlib-")
                .and_then(|s| s.strip_suffix(".jar"))
                .map(String::from)
        })
        .unwrap_or_else(|| "2.0.21".to_string())
}

/// Locate a dependency jar, downloading it from **Maven Central** into a local cache if not already
/// present (so `// WITH_STDLIB` assertions etc. actually have their jars). Returns `None` only if the
/// download fails (offline). Cached under `~/.cache/krusty-deps`.
#[allow(dead_code)]
pub fn ensure_maven(group: &str, artifact: &str, version: &str) -> Option<PathBuf> {
    let cache = std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".cache/krusty-deps"))?;
    let _ = std::fs::create_dir_all(&cache);
    let file = cache.join(format!("{artifact}-{version}.jar"));
    if file.is_file() {
        return Some(file);
    }
    let url = format!(
        "https://repo1.maven.org/maven2/{}/{artifact}/{version}/{artifact}-{version}.jar",
        group.replace('.', "/")
    );
    let status = std::process::Command::new("curl")
        .args(["-sfL", "--max-time", "60", "-o"])
        .arg(&file)
        .arg(&url)
        .status()
        .ok()?;
    if status.success() && file.is_file() {
        Some(file)
    } else {
        let _ = std::fs::remove_file(&file);
        None
    }
}

/// The set of `-classpath` jars a box test needs, formed from its directives — mirroring kotlinc's
/// `JvmEnvironmentConfigurator`: `WITH_STDLIB`/`WITH_RUNTIME` add kotlin-stdlib + kotlin-test +
/// annotations; `WITH_REFLECT` adds kotlin-reflect; `STDLIB_JDK8` adds kotlin-stdlib-jdk8;
/// `WITH_COROUTINES` adds kotlinx-coroutines-core. Missing jars are fetched from Maven Central.
#[allow(dead_code)]
pub fn classpath_jars_for(src: &str) -> Vec<PathBuf> {
    // The jar locations are constant; only the *directive set* of a file varies. Locating jars walks
    // the (huge) gradle/m2 caches recursively, so memoize per directive-signature — collapsing
    // thousands of ~1s filesystem walks into at most a handful.
    let sig: u8 = (directive(src, "WITH_STDLIB") as u8)
        | (directive(src, "WITH_RUNTIME") as u8) << 1
        | (directive(src, "WITH_REFLECT") as u8) << 2
        | (directive(src, "STDLIB_JDK8") as u8) << 3
        | (directive(src, "WITH_COROUTINES") as u8) << 4;
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<u8, Vec<PathBuf>>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Some(v) = cache.lock().unwrap().get(&sig) {
        return v.clone();
    }
    let jars = classpath_jars_uncached(src);
    cache.lock().unwrap().insert(sig, jars.clone());
    jars
}

fn classpath_jars_uncached(src: &str) -> Vec<PathBuf> {
    let mut jars = Vec::new();
    let v = kotlin_version();
    // `kotlinc` always puts kotlin-stdlib on the compile classpath (only `-no-stdlib` removes it), so a
    // faithful drop-in must too — supply it unconditionally, not just for `// WITH_STDLIB` tests. The
    // explicit directives still select the *extra* jars (reflect, jdk8, coroutines) below.
    let _ = (
        directive(src, "WITH_STDLIB"),
        directive(src, "WITH_RUNTIME"),
    );
    let with_stdlib = true;
    if with_stdlib {
        if let Some(j) = stdlib_jar() {
            jars.push(j);
        }
        if let Some(j) = kotlin_test_jar() {
            jars.push(j);
        }
        if let Some(j) = dist_jar("annotations-13.0.jar")
            .or_else(|| ensure_maven("org.jetbrains", "annotations", "23.0.0"))
        {
            jars.push(j);
        }
    }
    if directive(src, "WITH_REFLECT") {
        if let Some(j) = dist_jar("kotlin-reflect.jar")
            .or_else(|| ensure_maven("org.jetbrains.kotlin", "kotlin-reflect", &v))
        {
            jars.push(j);
        }
    }
    if directive(src, "STDLIB_JDK8") {
        if let Some(j) = dist_jar("kotlin-stdlib-jdk8.jar")
            .or_else(|| ensure_maven("org.jetbrains.kotlin", "kotlin-stdlib-jdk8", &v))
        {
            jars.push(j);
        }
    }
    if directive(src, "WITH_COROUTINES") {
        // Coroutines aren't in the dist; fetch the runtime jar from Maven.
        if let Some(j) = ensure_maven(
            "org.jetbrains.kotlinx",
            "kotlinx-coroutines-core-jvm",
            "1.9.0",
        ) {
            jars.push(j);
        }
    }
    jars
}

/// Locate a `kotlin-test` jar (`// WITH_STDLIB` adds it so `kotlin.test.*` resolves), from the local
/// caches or Maven Central.
#[allow(dead_code)]
pub fn kotlin_test_jar() -> Option<PathBuf> {
    dist_jar("kotlin-test.jar")
        .or_else(|| find_jar("kotlin-test-", &["junit", "testng", "annotations"]))
        .or_else(|| ensure_maven("org.jetbrains.kotlin", "kotlin-test", &kotlin_version()))
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

public class BoxRunner {
    static final long TIMEOUT_MS = 10000;
    static final ExecutorService EXEC = Executors.newCachedThreadPool(r -> {
        Thread t = new Thread(r);
        t.setDaemon(true);
        return t;
    });

    public static void main(String[] args) throws Exception {
        DataInputStream din = new DataInputStream(new BufferedInputStream(System.in, 65536));
        DataOutputStream dout = new DataOutputStream(new BufferedOutputStream(System.out, 4096));
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

/// A persistent JVM subprocess that accepts class bytes and runs `box()`.
struct BoxRunner {
    _child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
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
        let stdout = child.stdout.take()?;
        Some(BoxRunner {
            _child: child,
            stdin,
            stdout,
        })
    }

    fn try_run(
        &mut self,
        classes: &[(String, Vec<u8>)],
        box_class: &str,
    ) -> std::io::Result<String> {
        let n = classes.len() as u32;
        self.stdin.write_all(&n.to_be_bytes())?;
        for (name, data) in classes {
            self.stdin.write_all(&(name.len() as u16).to_be_bytes())?;
            self.stdin.write_all(name.as_bytes())?;
            self.stdin.write_all(&(data.len() as u32).to_be_bytes())?;
            self.stdin.write_all(data)?;
        }
        self.stdin
            .write_all(&(box_class.len() as u16).to_be_bytes())?;
        self.stdin.write_all(box_class.as_bytes())?;
        self.stdin.flush()?;

        let deadline = Instant::now() + Duration::from_secs(15);
        let fd = self.stdout.as_raw_fd();
        let mut len_buf = [0u8; 4];
        read_exact_deadline(fd, &mut len_buf, deadline)?;
        let rlen = u32::from_be_bytes(len_buf) as usize;
        let mut result = vec![0u8; rlen];
        read_exact_deadline(fd, &mut result, deadline)?;
        Ok(String::from_utf8_lossy(&result).into_owned())
    }
}

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
                    "BoxRunner EOF",
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

type RunnerPool = Mutex<HashMap<String, Mutex<BoxRunner>>>;

/// Run `box()` on already-compiled classes via a persistent JVM keyed by `cp_jars` (the runtime
/// classpath — typically the stdlib jar so loaded classes resolve `kotlin.jvm.internal.*`). Returns
/// the `box()` return value (or `ERROR:…`), or `None` if the JVM environment is unavailable.
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
    // First run with a key spins up its JVM; later runs reuse it. Hold the per-runner lock for the
    // round-trip (the protocol is request/response over one pipe, so it isn't concurrency-safe).
    {
        let mut map = pool.lock().unwrap();
        if !map.contains_key(&cp) {
            let runner = BoxRunner::new(&java, &cp)?;
            map.insert(cp.clone(), Mutex::new(runner));
        }
    }
    let map = pool.lock().unwrap();
    let runner_mx = map.get(&cp).unwrap();
    let mut runner = runner_mx.lock().unwrap();
    match runner.try_run(classes, box_class) {
        Ok(s) => Some(s),
        Err(_) => {
            // Subprocess died (e.g. a JVM crash); restart it once and retry.
            *runner = BoxRunner::new(&java, &cp)?;
            runner.try_run(classes, box_class).ok()
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
            .args(["-cp", cp, "KotlincServer"])
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

fn collect_stdlib_jars(dir: &std::path::Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 || out.len() > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_stdlib_jars(&p, out, depth + 1);
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("kotlin-stdlib-")
                && name.ends_with(".jar")
                && !name.contains("sources")
                && !name.contains("javadoc")
                && !name.contains("common")
                && !name.contains("-js")
                && !name.contains("wasm")
            {
                out.push(p);
            }
        }
    }
}
