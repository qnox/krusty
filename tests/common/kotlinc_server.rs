//! The persistent reference-`kotlinc` compiler server and its per-property pools.
//!
//! [`kotlinc_compile`] layers the recorded-byte archive over this live compiler;
//! [`kotlinc_compile_unrecorded`] is for a caller that owns its own content-keyed cache.
//! [`kotlinc_server_stats`] reports process-wide request, start, restart, pool-wait, and compile
//! time totals so a profiling run can tell compiler-server lifecycle cost from compile cost.

use std::cell::Cell;
use std::collections::HashMap;
use std::io::Write as _;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::server_pool::{self, server_pool_cap};
use super::{
    byte_dump, die_with_parent, java_home, jvm_gclog_args, kotlinc_lib_dir, read_exact_deadline,
    spawn_owned, ProfGuard,
};

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
    for b in KOTLINC_SERVER_SRC
        .bytes()
        .chain(super::producing_jdk::driver_cache_key(java_home))
    {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/kotlinc_server_{hash:016x}"));
    let class = dir.join("KotlincServer.class");
    if class.is_file() {
        return Some(dir);
    }
    // Coverage runs several test binaries at once. They share this cache, so the first
    // compile is exclusive; the others wait and reuse the class.
    std::fs::create_dir_all(&dir).ok()?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join("KotlincServer.lock"))
        .ok()?;
    // SAFETY: `flock` on a descriptor this function owns until it returns.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return None;
    }
    if class.is_file() {
        return Some(dir);
    }
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
    if !class.is_file() {
        eprintln!(
            "KotlincServer javac failed: status {} {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        return None;
    }
    Some(dir)
}

/// The reference compiler dist's lib dir (holds `kotlin-compiler.jar`) — the jars the isolating-loader
/// driver builds its per-compile `URLClassLoader` from. Passed to the driver as its argv[0].
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
    /// `jvm_properties` are `-D` arguments applied before the compiler class loads.
    fn new(java: &str, cp: &str, lib_dir: &str, jvm_properties: &[String]) -> Option<Self> {
        let mut cmd = Command::new(java);
        // One persistent JVM that compiles in-process. 1 GB holds a single compile's working set (the leaky
        // global state is reset after each — see the driver), so it stays flat across compiles. C1-only JIT
        // since each compile is short and the host's other cores are busy with the tests themselves; a full
        // tiered JIT measured no faster under that contention. The young generation is fixed instead of
        // left to grow from the small default initial heap: every compile allocates far more short-lived
        // data than it retains, and the undersized default nursery collected so often that a cold box
        // corpus shard spent ~19% more time in kotlinc (and grew a larger heap) than with 256 MB.
        cmd.args(jvm_gclog_args("kotlinc"));
        cmd.args([
            "-XX:TieredStopAtLevel=1",
            "-XX:+UseSerialGC",
            "-Xmx1g",
            "-Xmn256m",
        ]);
        cmd.args(jvm_properties);
        cmd.args(["-cp", cp, "KotlincServer", lib_dir])
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

/// Pools of persistent compiler servers, one per classpath and JVM-property set. Callers claim a
/// server, then compile outside the pool lock, so overlapping compiles use distinct JVMs up to
/// [`server_pool_cap`].
type KotlincPools = Mutex<HashMap<String, Arc<server_pool::Pool<KotlincServer>>>>;

/// Compile with the reference compiler via the persistent server. `args` are ordinary `kotlinc` CLI
/// arguments (`["-d", out, "-cp", cp, "Lib.kt"]`). Returns `(exit_code, stderr)` — `exit_code == 0`
/// is success — or `None` if the toolchain/JVM is unavailable (caller skips, exactly like a missing
/// `kotlinc`).
///
/// A release or RC replays class files, the exit code, and kotlinc's diagnostics from the
/// recorded-byte cache, for a successful build and for a rejected one. A test missing from that
/// archive fails locally. CI compiles it with kotlinc instead. Master stores that recording;
/// a pull request leaves the restored archive unchanged. A `-D` argument stays in the fingerprint.
/// On a cache miss the live compiler applies it as a JVM system property, on a server that no
/// other compile shares, and does not pass it through to the compiler.
pub fn kotlinc_compile(args: &[String]) -> Option<(i32, String)> {
    if let Some(replayed) = byte_dump::replay_class_dump(args) {
        if replayed.code == 0 {
            byte_dump::write_replayed_classes(args, &replayed.files);
        }
        return Some((replayed.code, replayed.stderr));
    }
    let result = kotlinc_compile_live(args)?;
    byte_dump::remember_class_dump(args, result.0, &result.1);
    Some(result)
}

/// [`kotlinc_compile`] without the recorded-byte archive: always the live persistent compiler, and
/// nothing is recorded. For a caller whose own cache is already keyed by every input and the exact
/// compiler identity, so a second archive layer would only duplicate its entries — and the archive
/// rewrites its whole in-memory body per stored entry and recompresses it at exit, which grows with
/// every entry a corpus-sized caller adds.
pub fn kotlinc_compile_unrecorded(args: &[String]) -> Option<(i32, String)> {
    kotlinc_compile_live(args)
}

/// Process-wide totals for the live compiler server, across every pool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KotlincServerStats {
    /// Compile requests that reached the live compiler (recorded-byte replays never do).
    pub requests: u64,
    /// Server JVMs spawned to grow a pool.
    pub starts: u64,
    /// Server JVMs respawned after one died mid-request.
    pub restarts: u64,
    /// Time requests spent waiting for an idle pooled server, excluding spawning a new one.
    pub pool_wait: Duration,
    /// Time spent spawning pooled server JVMs.
    pub start_time: Duration,
    /// Time from sending a request to its response, including a restart and its retry.
    pub compile_time: Duration,
}

static REQUESTS: AtomicU64 = AtomicU64::new(0);
static STARTS: AtomicU64 = AtomicU64::new(0);
static RESTARTS: AtomicU64 = AtomicU64::new(0);
static POOL_WAIT_NANOS: AtomicU64 = AtomicU64::new(0);
static START_NANOS: AtomicU64 = AtomicU64::new(0);
static COMPILE_NANOS: AtomicU64 = AtomicU64::new(0);

/// A snapshot of [`KotlincServerStats`] for this process.
pub fn kotlinc_server_stats() -> KotlincServerStats {
    let duration = |total: &AtomicU64| Duration::from_nanos(total.load(Ordering::Relaxed));
    KotlincServerStats {
        requests: REQUESTS.load(Ordering::Relaxed),
        starts: STARTS.load(Ordering::Relaxed),
        restarts: RESTARTS.load(Ordering::Relaxed),
        pool_wait: duration(&POOL_WAIT_NANOS),
        start_time: duration(&START_NANOS),
        compile_time: duration(&COMPILE_NANOS),
    }
}

fn add_elapsed(total: &AtomicU64, since: Instant) -> Duration {
    let elapsed = since.elapsed();
    total.fetch_add(
        u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
    elapsed
}

fn kotlinc_compile_live(args: &[String]) -> Option<(i32, String)> {
    static POOLS: OnceLock<KotlincPools> = OnceLock::new();
    let _pg = ProfGuard::new("kotlinc");
    REQUESTS.fetch_add(1, Ordering::Relaxed);
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
    // `-D` stays in `args` for the invocation fingerprint. The compiler itself rejects it, and a
    // test-only language feature latches from the JVM property the first time `-XXLanguage` is
    // parsed, so the property is applied at process start on a server that no other compile shares.
    let (jvm_properties, compiler_args) = kotlinc_jvm_properties(args);
    let pool_key = kotlinc_server_key(&cp, &jvm_properties);
    let pool = {
        let pools = POOLS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut pools = pools.lock().unwrap_or_else(|err| err.into_inner());
        pools
            .entry(pool_key)
            .or_insert_with(|| Arc::new(server_pool::Pool::new()))
            .clone()
    };
    let admission = Instant::now();
    let spawning = Cell::new(Duration::ZERO);
    pool.with_server(
        server_pool_cap(),
        || {
            let started = Instant::now();
            let server = KotlincServer::new(&java, &cp, &lib_dir, &jvm_properties);
            spawning.set(add_elapsed(&START_NANOS, started));
            STARTS.fetch_add(1, Ordering::Relaxed);
            server
        },
        |server| {
            let waited = admission.elapsed().saturating_sub(spawning.get());
            POOL_WAIT_NANOS.fetch_add(
                u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
            let compiling = Instant::now();
            let result = match server.try_compile(&compiler_args) {
                Ok(result) => Some(result),
                Err(_) => {
                    // Server JVM died — restart once and retry.
                    RESTARTS.fetch_add(1, Ordering::Relaxed);
                    match KotlincServer::new(&java, &cp, &lib_dir, &jvm_properties) {
                        Some(restarted) => {
                            *server = restarted;
                            server.try_compile(&compiler_args).ok()
                        }
                        None => None,
                    }
                }
            };
            add_elapsed(&COMPILE_NANOS, compiling);
            result
        },
    )
    .flatten()
}

/// Split `bin/kotlinc`'s `-D*` JVM system properties out of the compiler argument list.
///
/// `-destination` is the compiler's output-directory alias and stays a compiler argument.
fn kotlinc_jvm_properties(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut properties = Vec::new();
    let mut compiler_args = Vec::new();
    for arg in args {
        if arg.starts_with("-D") && arg != "-destination" {
            properties.push(arg.clone());
        } else {
            compiler_args.push(arg.clone());
        }
    }
    (properties, compiler_args)
}

/// The default server is keyed by its classpath alone. A JVM property gets its own server, created
/// only when a cache miss reaches the live compiler.
fn kotlinc_server_key(classpath: &str, properties: &[String]) -> String {
    if properties.is_empty() {
        return classpath.to_string();
    }
    let mut key = String::from(classpath);
    for property in properties {
        key.push('\0');
        key.push_str(property);
    }
    key
}

#[cfg(test)]
mod stats_tests {
    use super::{kotlinc_compile_unrecorded, kotlinc_server_stats};

    // Other tests in this process compile concurrently, so the totals can only be bounded below.
    #[test]
    fn an_unrecorded_compile_is_counted_in_the_cumulative_server_totals() {
        let root = super::super::scratch_dir().expect("allocate a compile directory");
        let source = root.join("Counted.kt");
        std::fs::write(&source, "fun counted() = 1\n").expect("write the source");
        let before = kotlinc_server_stats();
        let args = [
            "-d".to_string(),
            root.join("classes").to_string_lossy().into_owned(),
            source.to_string_lossy().into_owned(),
        ];
        let (code, diagnostics) =
            kotlinc_compile_unrecorded(&args).expect("reference compiler is provisioned");
        let after = kotlinc_server_stats();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!((code, diagnostics.as_str()), (0, ""));
        assert!(after.requests > before.requests);
        assert!(after.compile_time > before.compile_time);
        assert!(after.starts >= 1 && after.start_time > std::time::Duration::ZERO);
        assert!(after.restarts >= before.restarts);
        assert!(after.pool_wait >= before.pool_wait);
    }
}
