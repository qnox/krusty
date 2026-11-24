//! A persistent `kotlinc-native` for the reference oracles.
//!
//! The `kotlinc-native` launcher starts a fresh JVM for every invocation, on a C1-only JIT with a
//! 3 GB heap. A multimodule oracle case compiles each library and then the program, so that cold
//! start dominated it: a warm in-process compile of a small library took about 0.2 s instead of
//! 2.8 s, and a cached program 0.6 s instead of 4.6 s. Kotlin's Gradle plugin runs this compiler
//! in its daemon the same way.
//!
//! The driver does what the launcher's `konanc` entry does for one invocation, inside one JVM:
//! it brackets the compile with the native allocator's dispose helper and runs
//! `K2Native.exec(stderr, args)`, whose diagnostics use the same renderer as the command line. A
//! server serves one compile at a time; [`server_pool_cap`] servers run side by side.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::server_pool::{self, server_pool_cap};
use super::{
    die_with_parent, java_home, protocol_jvm_log_args, read_exact_deadline, spawn_owned,
    timeout_ms_from,
};

const KOTLIN_NATIVE_SERVER_SRC: &str = r#"
import java.io.*;
import java.nio.charset.StandardCharsets;

public class KotlinNativeServer {
    public static void main(String[] a) throws Exception {
        DataInputStream in = new DataInputStream(new BufferedInputStream(new FileInputStream(FileDescriptor.in), 65536));
        DataOutputStream out = new DataOutputStream(new BufferedOutputStream(new FileOutputStream(FileDescriptor.out), 4096));
        // The protocol owns the raw stdout; anything the compiler prints there goes to stderr.
        System.setOut(System.err);
        while (true) {
            int n;
            try { n = in.readInt(); } catch (EOFException e) { break; }
            String[] args = new String[n];
            for (int i = 0; i < n; i++) {
                args[i] = new String(in.readNBytes(in.readInt()), StandardCharsets.UTF_8);
            }
            ByteArrayOutputStream errBuf = new ByteArrayOutputStream();
            PrintStream err = new PrintStream(errBuf, true, "UTF-8");
            int code;
            org.jetbrains.kotlin.utils.ThreadSafeDisposableHelper allocator =
                org.jetbrains.kotlin.utils.NativeMemoryAllocatorKt.getAllocatorDisposeHelper();
            allocator.create();
            try {
                code = new org.jetbrains.kotlin.cli.bc.K2Native().exec(err, args).getCode();
            } catch (Throwable t) {
                t.printStackTrace(err);
                code = 2;
            } finally {
                allocator.dispose();
            }
            byte[] bytes = errBuf.toByteArray();
            out.writeInt(code);
            out.writeInt(bytes.length);
            out.write(bytes);
            out.flush();
        }
    }
}
"#;

/// The launcher's JVM options (`bin/run_konan`), plus a code cache that a long-lived C1 JVM does
/// not fill: with the default 48 MB the JVM disables its JIT for good after a few hundred compiles.
const KOTLIN_NATIVE_SERVER_JVM_OPTIONS: [&str; 5] = [
    "-ea",
    "-Xmx3G",
    "-XX:TieredStopAtLevel=1",
    "-XX:ReservedCodeCacheSize=256m",
    "-Dfile.encoding=UTF-8",
];

/// Milliseconds one compile may take before its server counts as hung and is replaced:
/// `KRUSTY_NATIVE_COMPILE_TIMEOUT_MS`, five minutes by default. No compile in the oracle corpus
/// takes a minute warm; slower or busier hosts raise it.
fn compile_deadline() -> Duration {
    static TIMEOUT: OnceLock<u64> = OnceLock::new();
    Duration::from_millis(*TIMEOUT.get_or_init(|| {
        compile_timeout_ms_from(
            std::env::var("KRUSTY_NATIVE_COMPILE_TIMEOUT_MS")
                .ok()
                .as_deref(),
        )
    }))
}

fn compile_timeout_ms_from(raw: Option<&str>) -> u64 {
    timeout_ms_from("KRUSTY_NATIVE_COMPILE_TIMEOUT_MS", raw, 300_000)
}

/// What one `kotlinc-native` invocation produced.
pub(crate) struct NativeCompile {
    pub(crate) status: i32,
    pub(crate) stderr: String,
}

/// Run `kotlinc-native` from the distribution at `root` with `args`, in a persistent server.
pub(crate) fn kotlinc_native(root: &Path, args: &[OsString]) -> NativeCompile {
    type Pools = Mutex<HashMap<PathBuf, Arc<server_pool::Pool<KotlinNativeServer>>>>;
    static POOLS: OnceLock<Pools> = OnceLock::new();
    let pool = Arc::clone(
        POOLS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(root.to_path_buf())
            .or_insert_with(|| Arc::new(server_pool::Pool::new())),
    );
    let start = || KotlinNativeServer::start(root);
    pool.with_server(
        server_pool_cap(),
        || Some(start()),
        |server| match server.compile(args) {
            Ok(compile) => compile,
            Err(first) => {
                *server = start();
                server.compile(args).unwrap_or_else(|second| {
                    panic!("kotlinc-native server failed twice: {first}; then {second}")
                })
            }
        },
    )
    .expect("a kotlinc-native server is admitted")
}

struct KotlinNativeServer {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl KotlinNativeServer {
    fn start(root: &Path) -> Self {
        let jar = root.join("konan/lib/kotlin-native-compiler-embeddable.jar");
        assert!(
            jar.is_file(),
            "the Kotlin/Native distribution has no compiler at {}",
            jar.display()
        );
        let java_home = java_home();
        let classes = driver_classes(&java_home, &jar);
        let mut classpath = OsString::from(&jar);
        classpath.push(":");
        classpath.push(&classes);
        let mut command = Command::new(format!("{java_home}/bin/java"));
        command
            .args(protocol_jvm_log_args("kotlinc-native"))
            .args(KOTLIN_NATIVE_SERVER_JVM_OPTIONS)
            .arg("--enable-native-access=ALL-UNNAMED")
            .arg({
                let mut home = OsString::from("-Dkonan.home=");
                home.push(root);
                home
            })
            .arg("-cp")
            .arg(classpath)
            .arg("KotlinNativeServer")
            // The launcher's environment: clang crash recovery off, Xcode build variables unset.
            .env("LIBCLANG_DISABLE_CRASH_RECOVERY", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let blacklist = root.join("tools/env_blacklist");
        let blacklist = std::fs::read_to_string(&blacklist)
            .unwrap_or_else(|error| panic!("read {}: {error}", blacklist.display()));
        for name in blacklist
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            command.env_remove(name);
        }
        die_with_parent(&mut command);
        let mut child = spawn_owned(command)
            .unwrap_or_else(|error| panic!("start the kotlinc-native server: {error}"));
        let stdin = child.stdin.take().expect("a piped stdin");
        let stdout = child.stdout.take().expect("a piped stdout");
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn compile(&mut self, args: &[OsString]) -> std::io::Result<NativeCompile> {
        let count = u32::try_from(args.len()).expect("a representable argument count");
        self.stdin.write_all(&count.to_be_bytes())?;
        for arg in args {
            let bytes = arg.as_bytes();
            let len = u32::try_from(bytes.len()).expect("a representable argument");
            self.stdin.write_all(&len.to_be_bytes())?;
            self.stdin.write_all(bytes)?;
        }
        self.stdin.flush()?;
        let deadline = Instant::now() + compile_deadline();
        let fd = self.stdout.as_raw_fd();
        let mut word = [0u8; 4];
        read_exact_deadline(fd, &mut word, deadline)?;
        let status = i32::from_be_bytes(word);
        read_exact_deadline(fd, &mut word, deadline)?;
        let mut stderr = vec![0u8; u32::from_be_bytes(word) as usize];
        read_exact_deadline(fd, &mut stderr, deadline)?;
        Ok(NativeCompile {
            status,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

impl Drop for KotlinNativeServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Compile the driver once per JDK and compiler jar into a shared cache directory. Test binaries
/// that run at once share it, so the first compile is exclusive and the others reuse its class.
fn driver_classes(java_home: &str, jar: &Path) -> PathBuf {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in KOTLIN_NATIVE_SERVER_SRC
        .bytes()
        .chain(super::producing_jdk::driver_cache_key(java_home))
        .chain(jar.as_os_str().as_bytes().iter().copied())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("target/kotlin_native_server_{hash:016x}"));
    let class = dir.join("KotlinNativeServer.class");
    if class.is_file() {
        return dir;
    }
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|error| panic!("create {}: {error}", dir.display()));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join("KotlinNativeServer.lock"))
        .unwrap_or_else(|error| panic!("open the kotlinc-native driver lock: {error}"));
    // SAFETY: `flock` on a descriptor this function owns until it returns.
    let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(locked, 0, "lock the kotlinc-native driver directory");
    if class.is_file() {
        return dir;
    }
    let source = dir.join("KotlinNativeServer.java");
    std::fs::write(&source, KOTLIN_NATIVE_SERVER_SRC)
        .unwrap_or_else(|error| panic!("write {}: {error}", source.display()));
    let output = Command::new(format!("{java_home}/bin/javac"))
        .arg("-proc:none")
        .arg("-cp")
        .arg(jar)
        .arg("-d")
        .arg(&dir)
        .arg(&source)
        .output()
        .unwrap_or_else(|error| panic!("run javac for the kotlinc-native driver: {error}"));
    assert!(
        class.is_file(),
        "javac rejected the kotlinc-native driver ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    dir
}

#[test]
fn the_compile_deadline_defaults_to_five_minutes_and_follows_its_variable() {
    assert_eq!(compile_timeout_ms_from(None), 300_000);
    assert_eq!(compile_timeout_ms_from(Some(" ")), 300_000);
    assert_eq!(compile_timeout_ms_from(Some("900000")), 900_000);
    assert!(std::panic::catch_unwind(|| compile_timeout_ms_from(Some("0"))).is_err());
}
