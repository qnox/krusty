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
    byte_dump, die_with_parent, java_home, kotlinc_lib_dir, protocol_jvm_log_args,
    read_exact_deadline, spawn_owned, ProfGuard,
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
import java.lang.reflect.Field;

public class KotlincServer {
    public static void main(String[] a) throws Exception {
        // Compiler jars for the loader — drop `-sources`/JS/WASM jars (not needed to compile plain JVM
        // Kotlin) so the one loader holds less. The scripting jars stay out as well, as they do on
        // kotlinc's own compiler classpath: the compiler loads its scripting plugin from the
        // distribution, and a script template on the compilation classpath must see that plugin's
        // scripting API, not a second copy in this loader.
        File[] files = new File(a[0]).listFiles();
        ArrayList<URL> urls = new ArrayList<>();
        if (files != null) for (File f : files) {
            String n = f.getName();
            if (n.endsWith(".jar") && !n.endsWith("-sources.jar") && !n.contains("-js") && !n.contains("-wasm")
                    && !n.contains("scripting"))
                urls.add(f.toURI().toURL());
        }
        urls.add(new File(a[1]).toURI().toURL());
        // ONE loader for the whole session: the compiler classes load once (this is the warmth). Parent is
        // the platform loader only, so the compiler's classes stay private to it.
        URLClassLoader cl = new URLClassLoader(urls.toArray(new URL[0]), ClassLoader.getPlatformClassLoader());
        Class<?> k = cl.loadClass(a[2]);
        Method exec = k.getMethod("exec", PrintStream.class, String[].class);
        Class<?> typedSettings = cl.loadClass("TypedLanguageSettingsUpdater");
        Object typedSettingsUpdater = typedSettings.getDeclaredConstructor().newInstance();
        Method selectLanguageSettings = typedSettings.getMethod("select", String.class, String.class);
        Object configurationPhase = cl
            .loadClass(a[3])
            .getField("INSTANCE").get(null);
        Field updatersField = cl.loadClass("org.jetbrains.kotlin.cli.pipeline.AbstractConfigurationPhase")
            .getDeclaredField("configurationUpdaters");
        updatersField.setAccessible(true);
        ArrayList<Object> updaters = new ArrayList<>((List<?>) updatersField.get(configurationPhase));
        updaters.add(typedSettingsUpdater);
        updatersField.set(configurationPhase, Collections.unmodifiableList(updaters));
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
            String languageVersion = readOptionalString(din);
            String apiVersion = readOptionalString(din);
            String[] args = new String[n];
            for (int i = 0; i < n; i++) {
                int l = din.readUnsignedShort();
                args[i] = new String(din.readNBytes(l), "UTF-8");
            }
            ByteArrayOutputStream errBuf = new ByteArrayOutputStream();
            PrintStream err = new PrintStream(errBuf, true, "UTF-8");
            int codeNum;
            try {
                selectLanguageSettings.invoke(null, languageVersion, apiVersion);
                Object comp = k.getDeclaredConstructor().newInstance();
                Object code = exec.invoke(comp, err, (Object) args);
                codeNum = (int) code.getClass().getMethod("getCode").invoke(code);
            } catch (Throwable t) {
                t.printStackTrace(err);
                codeNum = 2;
            } finally {
                try { selectLanguageSettings.invoke(null, null, null); } catch (Throwable ignore) {}
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

    private static String readOptionalString(DataInputStream in) throws IOException {
        int length = in.readUnsignedShort();
        return length == 0 ? null : new String(in.readNBytes(length), "UTF-8");
    }
}
"#;

const TYPED_LANGUAGE_SETTINGS_UPDATER_SRC: &str = r#"
import java.util.Collections;
import java.util.Map;
import org.jetbrains.kotlin.cli.common.arguments.CommonCompilerArguments;
import org.jetbrains.kotlin.cli.pipeline.ArgumentsPipelineArtifact;
import org.jetbrains.kotlin.cli.pipeline.ConfigurationUpdater;
import org.jetbrains.kotlin.config.AnalysisFlag;
import org.jetbrains.kotlin.config.ApiVersion;
import org.jetbrains.kotlin.config.CommonConfigurationKeys;
import org.jetbrains.kotlin.config.CompilerConfiguration;
import org.jetbrains.kotlin.config.LanguageFeature;
import org.jetbrains.kotlin.config.LanguageVersion;
import org.jetbrains.kotlin.config.LanguageVersionSettings;
import org.jetbrains.kotlin.config.LanguageVersionSettingsImpl;

public final class TypedLanguageSettingsUpdater extends ConfigurationUpdater<CommonCompilerArguments> {
    private static String selectedLanguageVersion;
    private static String selectedApiVersion;

    public static void select(String languageVersion, String apiVersion) {
        selectedLanguageVersion = languageVersion;
        selectedApiVersion = apiVersion;
    }

    @Override
    public void fillConfiguration(
        ArgumentsPipelineArtifact<? extends CommonCompilerArguments> input,
        CompilerConfiguration configuration
    ) {
        if (selectedLanguageVersion == null && selectedApiVersion == null) return;
        LanguageVersionSettings original = configuration.get(CommonConfigurationKeys.LANGUAGE_VERSION_SETTINGS);
        if (original == null) throw new IllegalStateException("kotlinc did not configure language settings");
        LanguageVersion language = selectedLanguageVersion == null
            ? original.getLanguageVersion()
            : LanguageVersion.fromVersionString(selectedLanguageVersion);
        if (language == null) throw new IllegalArgumentException("unknown language version " + selectedLanguageVersion);
        ApiVersion api = selectedApiVersion == null
            ? (selectedLanguageVersion == null
                ? original.getApiVersion()
                : ApiVersion.createByLanguageVersion(language))
            : ApiVersion.Companion.parse(selectedApiVersion);
        if (api == null) throw new IllegalArgumentException("unknown API version " + selectedApiVersion);
        if (api.compareTo(ApiVersion.createByLanguageVersion(language)) > 0) {
            throw new IllegalArgumentException("API version " + api + " exceeds language version " + language);
        }
        LanguageVersionSettings adjusted = new LanguageVersionSettingsImpl(
            language,
            api,
            Collections.emptyMap(),
            original.getCustomizedLanguageFeatures()
        );
        configuration.put(
            CommonConfigurationKeys.LANGUAGE_VERSION_SETTINGS,
            new PreservingSettings(adjusted, original)
        );
    }

    private static final class PreservingSettings implements LanguageVersionSettings {
        private final LanguageVersionSettings adjusted;
        private final LanguageVersionSettings original;

        PreservingSettings(LanguageVersionSettings adjusted, LanguageVersionSettings original) {
            this.adjusted = adjusted;
            this.original = original;
        }
        public LanguageFeature.State getFeatureSupport(LanguageFeature feature) {
            return adjusted.getFeatureSupport(feature);
        }
        public Map<LanguageFeature, LanguageFeature.State> getCustomizedLanguageFeatures() {
            return adjusted.getCustomizedLanguageFeatures();
        }
        public boolean isPreRelease() { return adjusted.isPreRelease(); }
        public <T> T getFlag(AnalysisFlag<? extends T> flag) { return original.getFlag(flag); }
        public ApiVersion getApiVersion() { return adjusted.getApiVersion(); }
        public LanguageVersion getLanguageVersion() { return adjusted.getLanguageVersion(); }
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
fn setup_kotlinc_server(java_home: &str, compiler_jar: &Path) -> Option<PathBuf> {
    // The JDK is part of the cache key: a class compiled by a NEWER javac (another session's
    // JAVA_HOME) is unloadable by an older runtime, and the server-spawn failure then reads as
    // "kotlinc unavailable" — silently disabling every reference compile and cross-check.
    // The compiler jar is part of it too: the settings updater is compiled against that
    // distribution and loaded by its class loader, so two releases do not share one class file.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in KOTLINC_SERVER_SRC
        .bytes()
        .chain(TYPED_LANGUAGE_SETTINGS_UPDATER_SRC.bytes())
        .chain(super::producing_jdk::driver_cache_key(java_home))
        .chain(compiler_jar.to_string_lossy().bytes())
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
    let updater_src_path = dir.join("TypedLanguageSettingsUpdater.java");
    std::fs::write(&src_path, KOTLINC_SERVER_SRC).ok()?;
    std::fs::write(&updater_src_path, TYPED_LANGUAGE_SETTINGS_UPDATER_SRC).ok()?;
    let javac = format!("{java_home}/bin/javac");
    if !Path::new(&javac).exists() {
        return None;
    }
    let out = Command::new(&javac)
        .arg("-cp")
        .arg(format!("{}/*", compiler_jar.parent()?.to_string_lossy()))
        .arg("-d")
        .arg(&dir)
        .arg(&src_path)
        .arg(&updater_src_path)
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

/// The reference compiler server's JVM options; [`KotlincServer::new`] explains the heap and JIT.
///
/// A C1-only JVM reserves a 48 MB code cache. The compiler's code fills it after about 200 warm
/// compiles, and the JVM then disables its JIT for good ("CodeCache is full. Compiler has been
/// disabled"), so a cold box corpus ran nearly all of its ~7,400 reference compiles interpreted.
/// 808 box compiles in one server took 205 s (264 s CPU) with the default cache and 140 s (149 s
/// CPU) with 256 MB, of which about 70 MB is used and stays flat over 2,000 compiles. The full tiered
/// JIT was faster per compile (111 s) but cost 213 s CPU, which the busy test host pays for.
const KOTLINC_SERVER_JVM_OPTIONS: [&str; 5] = [
    "-XX:TieredStopAtLevel=1",
    "-XX:ReservedCodeCacheSize=256m",
    "-XX:+UseSerialGC",
    "-Xmx1g",
    "-Xmn256m",
];

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
    fn new(
        java: &str,
        cp: &str,
        compiler: &SelectedCompiler,
        jvm_properties: &[String],
    ) -> Option<Self> {
        let mut cmd = Command::new(java);
        // One persistent JVM that compiles in-process. 1 GB holds a single compile's working set (the leaky
        // global state is reset after each — see the driver), so it stays flat across compiles. C1-only JIT
        // since each compile is short and the host's other cores are busy with the tests themselves; a full
        // tiered JIT measured no faster under that contention. The young generation is fixed instead of
        // left to grow from the small default initial heap: every compile allocates far more short-lived
        // data than it retains, and the undersized default nursery collected so often that a cold box
        // corpus shard spent ~19% more time in kotlinc (and grew a larger heap) than with 256 MB.
        cmd.args(protocol_jvm_log_args("kotlinc"));
        cmd.args(KOTLINC_SERVER_JVM_OPTIONS);
        cmd.args(jvm_properties);
        cmd.args([
            "-cp",
            cp,
            "KotlincServer",
            &compiler.lib_dir,
            cp,
            compiler.class,
            compiler.phase,
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

    fn try_compile(
        &mut self,
        args: &[String],
        language_version: Option<&str>,
        api_version: Option<&str>,
    ) -> std::io::Result<(i32, String)> {
        self.stdin.write_all(&(args.len() as u32).to_be_bytes())?;
        for version in [language_version, api_version] {
            let bytes = version.unwrap_or_default().as_bytes();
            self.stdin.write_all(&(bytes.len() as u16).to_be_bytes())?;
            self.stdin.write_all(bytes)?;
        }
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
    let jar = kotlin_compiler_jar()?;
    let release = krusty::kotlin_version::target().to_string();
    // The provisioned distribution of the process release is that compiler. An override
    // (`KRUSTY_KOTLINC`) names a different jar and keeps this function on that jar.
    if compiler_jar_for_release(&release).as_deref() == Some(jar.as_ref()) {
        return kotlinc_compile_for_release(&release, args);
    }
    compile_jar(&jar, args)
}

/// Compile `args` with the kotlinc distribution of `release`, through the same server and
/// recorded-byte replay as [`kotlinc_compile`].
///
/// A release or RC replays under that distribution's identity and `lib/` fingerprint. A beta
/// compiles live and is not archived. The distribution is
/// `target/cache/kotlinc/<release>/`, downloaded when it is absent. The process compiler, the one
/// [`kotlinc_compile`] uses, keeps its own replay key.
pub fn kotlinc_compile_for_release(release: &str, args: &[String]) -> Option<(i32, String)> {
    compile_jar(&compiler_jar_for_release(release)?, args)
}

fn compile_jar(compiler_jar: &Path, args: &[String]) -> Option<(i32, String)> {
    let lib = compiler_jar.parent()?;
    // The process compiler keeps its existing replay key. Another release fingerprints that
    // release's `lib/` and build identity, and a beta compiles live.
    let process_compiler = kotlin_compiler_jar().as_deref() == Some(compiler_jar);
    let identity = build_identity(lib);
    let replayed = if process_compiler {
        byte_dump::replay_class_dump(args)
    } else {
        byte_dump::replay_class_dump_for_compiler(args, identity.as_deref(), Some(lib))
    };
    if let Some(replayed) = replayed {
        if replayed.code == 0 {
            byte_dump::write_replayed_classes(args, &replayed.files);
        }
        return Some((replayed.code, replayed.stderr));
    }
    let result = kotlinc_compile_with_jar(compiler_jar, args, None, None)?;
    if process_compiler {
        byte_dump::report_live_kotlinc(args);
        byte_dump::remember_class_dump(args, result.0, &result.1);
    } else {
        byte_dump::report_live_kotlinc_for(args, identity.is_some(), Some(lib));
        byte_dump::remember_class_dump_for_compiler(
            args,
            result.0,
            &result.1,
            identity.as_deref(),
            Some(lib),
        );
    }
    Some(result)
}

/// The `lib/` directory of `release`'s kotlinc distribution, provisioning it when needed.
pub fn kotlinc_lib_for_release(release: &str) -> Option<PathBuf> {
    let jar = cached_compiler_jar(release);
    if jar.is_file() {
        return jar.parent().map(Path::to_path_buf);
    }
    if release == krusty::kotlin_version::target().to_string() {
        if let Some(jar) = kotlin_compiler_jar() {
            return jar.parent().map(Path::to_path_buf);
        }
    }
    provision_kotlinc(release).and_then(|jar| jar.parent().map(Path::to_path_buf))
}

fn compiler_jar_for_release(release: &str) -> Option<PathBuf> {
    let jar = kotlinc_lib_for_release(release)?.join("kotlin-compiler.jar");
    jar.is_file().then_some(jar)
}

fn cached_compiler_jar(release: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "target/cache/kotlinc/{release}/kotlinc/lib/kotlin-compiler.jar"
    ))
}

fn build_identity(lib: &Path) -> Option<String> {
    let text = std::fs::read_to_string(lib.parent()?.join("build.txt")).ok()?;
    let line = text.lines().next()?.trim();
    byte_dump::cacheable_build_identity(line)
}

fn provision_kotlinc(release: &str) -> Option<PathBuf> {
    let dest =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/cache/kotlinc/{release}"));
    let jar = dest.join("kotlinc/lib/kotlin-compiler.jar");
    std::fs::create_dir_all(&dest).ok()?;
    let lock_path = dest.join(".provision.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    // SAFETY: `flock` on a descriptor this function owns until it returns.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return None;
    }
    if jar.is_file() {
        return Some(jar);
    }
    let url = format!(
        "https://github.com/JetBrains/kotlin/releases/download/v{release}/kotlin-compiler-{release}.zip"
    );
    let tmp = std::env::temp_dir().join(format!("kotlinc-{release}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).ok()?;
    let zip = tmp.join("kotlinc.zip");
    let downloaded = Command::new("curl")
        .args(["-fsSL", &url, "-o"])
        .arg(&zip)
        .status()
        .ok()
        .is_some_and(|status| status.success());
    if !downloaded {
        let _ = std::fs::remove_dir_all(&tmp);
        return None;
    }
    let unpacked = Command::new("unzip")
        .args(["-q"])
        .arg(&zip)
        .arg("-d")
        .arg(&dest)
        .status()
        .ok()
        .is_some_and(|status| status.success());
    let _ = std::fs::remove_dir_all(&tmp);
    if !unpacked || !jar.is_file() {
        return None;
    }
    let bin = dest.join("kotlinc/bin/kotlinc");
    let _ = std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    Some(jar)
}

/// [`kotlinc_compile`] without the recorded-byte archive: always the live persistent compiler, and
/// nothing is recorded. For a caller whose own cache is already keyed by every input and the exact
/// compiler identity, so a second archive layer would only duplicate its entries — and the archive
/// rewrites its whole in-memory body per stored entry and recompresses it at exit, which grows with
/// every entry a corpus-sized caller adds.
///
/// Every call reaches the JVM, so every completed compile is reported like a recorded-path miss.
pub fn kotlinc_compile_unrecorded(args: &[String]) -> Option<(i32, String)> {
    kotlinc_compile_unrecorded_with_language_settings(args, None, None)
}

/// Compile with exact language/API levels installed in kotlinc's typed compiler configuration.
/// This mirrors Kotlin's own test runner for historical levels which current kotlinc releases no
/// longer accept as public command-line options.
pub fn kotlinc_compile_unrecorded_with_language_settings(
    args: &[String],
    language_version: Option<&str>,
    api_version: Option<&str>,
) -> Option<(i32, String)> {
    let result = kotlinc_compile_live(args, language_version, api_version)?;
    byte_dump::report_unrecorded_kotlinc(args);
    Some(result)
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

fn kotlinc_compile_live(
    args: &[String],
    language_version: Option<&str>,
    api_version: Option<&str>,
) -> Option<(i32, String)> {
    kotlinc_compile_for_target(
        krusty::compilation_target::CompilationTarget::Jvm,
        args,
        language_version,
        api_version,
    )
}

/// The JVM compiler of one named release, in the same pool machinery as the process compiler.
fn kotlinc_compile_with_jar(
    compiler_jar: &Path,
    args: &[String],
    language_version: Option<&str>,
    api_version: Option<&str>,
) -> Option<(i32, String)> {
    kotlinc_compile_on(
        krusty::compilation_target::CompilationTarget::Jvm,
        Some(compiler_jar),
        args,
        language_version,
        api_version,
    )
}

struct SelectedCompiler {
    lib_dir: String,
    class: &'static str,
    phase: &'static str,
}

/// The same bounded persistent compiler pool for JVM, JS, Wasm and Native library oracles.
/// Compiler classes and typed configuration phases are selected explicitly, never retried as JVM.
pub fn kotlinc_compile_for_target(
    target: krusty::compilation_target::CompilationTarget,
    args: &[String],
    language_version: Option<&str>,
    api_version: Option<&str>,
) -> Option<(i32, String)> {
    kotlinc_compile_on(target, None, args, language_version, api_version)
}

/// `jvm_jar` names another release's JVM compiler; it applies to the JVM target only.
fn kotlinc_compile_on(
    target: krusty::compilation_target::CompilationTarget,
    jvm_jar: Option<&Path>,
    args: &[String],
    language_version: Option<&str>,
    api_version: Option<&str>,
) -> Option<(i32, String)> {
    static POOLS: OnceLock<KotlincPools> = OnceLock::new();
    let _pg = ProfGuard::new("kotlinc");
    REQUESTS.fetch_add(1, Ordering::Relaxed);
    let java_home = java_home();
    let java = format!("{java_home}/bin/java");
    if !Path::new(&java).exists() {
        return None;
    }
    use krusty::compilation_target::CompilationTarget;
    let mut properties = Vec::new();
    let (compiler_jar, class, phase) = match target {
        CompilationTarget::Jvm => (
            match jvm_jar {
                Some(jar) => jar.to_path_buf(),
                None => kotlin_compiler_jar()?,
            },
            "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler",
            "org.jetbrains.kotlin.cli.pipeline.jvm.JvmConfigurationPipelinePhase",
        ),
        CompilationTarget::Js => (
            kotlin_compiler_jar()?,
            "org.jetbrains.kotlin.cli.js.K2JSCompiler",
            "org.jetbrains.kotlin.cli.pipeline.web.JsConfigurationPhase",
        ),
        CompilationTarget::WasmJs | CompilationTarget::WasmWasi => (
            kotlin_compiler_jar()?,
            "org.jetbrains.kotlin.cli.js.KotlinWasmCompiler",
            "org.jetbrains.kotlin.cli.pipeline.web.WasmConfigurationPhase",
        ),
        CompilationTarget::Native => {
            let root = PathBuf::from(std::env::var_os("KRUSTY_KOTLIN_NATIVE")?);
            properties.push(format!("-Dkonan.home={}", root.display()));
            (
                root.join("konan/lib/kotlin-native-compiler-embeddable.jar"),
                "org.jetbrains.kotlin.cli.bc.K2Native",
                "org.jetbrains.kotlin.native.pipeline.NativeConfigurationPhase",
            )
        }
    };
    let server_dir = setup_kotlinc_server(&java_home, &compiler_jar)?;
    let compiler = SelectedCompiler {
        lib_dir: kotlinc_lib_of(&compiler_jar)?
            .to_string_lossy()
            .into_owned(),
        class,
        phase,
    };
    let cp = server_dir.to_string_lossy().into_owned();
    let (jvm_properties, compiler_args) = kotlinc_jvm_properties(args);
    properties.extend(jvm_properties);
    // A compiler kind/distribution/property set owns its own pool and class loader.
    let pool_identity = format!("{cp}\0{}\0{}", compiler.lib_dir, compiler.class);
    let pool_key = kotlinc_server_key(&pool_identity, &properties);
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
            let server = KotlincServer::new(&java, &cp, &compiler, &properties);
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
            let result = match server.try_compile(&compiler_args, language_version, api_version) {
                Ok(result) => Some(result),
                Err(_) => {
                    // Server JVM died — restart once and retry.
                    RESTARTS.fetch_add(1, Ordering::Relaxed);
                    match KotlincServer::new(&java, &cp, &compiler, &properties) {
                        Some(restarted) => {
                            *server = restarted;
                            server
                                .try_compile(&compiler_args, language_version, api_version)
                                .ok()
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

    // A forced re-record (`KRUSTY_RECORD=1`) does not apply to a caller-owned cache, so the reason
    // is the same whether or not the variable is set.
    #[test]
    fn an_unrecorded_compile_reports_one_live_kotlinc_cache_miss_for_its_test() {
        let test = std::thread::current()
            .name()
            .expect("libtest names the test thread")
            .to_string();
        let root = super::super::scratch_dir().expect("allocate a compile directory");
        let source = root.join("Reported.kt");
        std::fs::write(&source, "fun reported() = 1\n").expect("write the source");
        let args = [
            "-d".to_string(),
            root.join("classes").to_string_lossy().into_owned(),
            source.to_string_lossy().into_owned(),
        ];
        assert_eq!(
            super::byte_dump::live_kotlinc_reasons(&test),
            Vec::<&str>::new()
        );
        let (code, diagnostics) =
            kotlinc_compile_unrecorded(&args).expect("reference compiler is provisioned");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!((code, diagnostics.as_str()), (0, ""));
        let expected = if super::byte_dump::published_compiler_id().is_some() {
            "cache-miss"
        } else {
            "uncached-compiler"
        };
        assert_eq!(
            super::byte_dump::live_kotlinc_reasons(&test),
            vec![expected]
        );
    }
}

#[cfg(test)]
mod jvm_option_tests {
    use super::KOTLINC_SERVER_JVM_OPTIONS;

    /// The C1-only server must keep its JIT across a whole cold corpus: the JVM's default code cache
    /// for that mode fills partway through and disables compilation for the rest of the process.
    #[test]
    fn the_reference_server_reserves_a_code_cache_that_outlasts_a_cold_corpus() {
        let java = format!("{}/bin/java", super::super::java_home());
        let output = std::process::Command::new(java)
            .args(KOTLINC_SERVER_JVM_OPTIONS)
            .args(["-XX:+PrintFlagsFinal", "-version"])
            .output()
            .expect("run the reference JVM");
        assert!(
            output.status.success(),
            "the reference JVM rejected its options"
        );
        let flags = String::from_utf8_lossy(&output.stdout);
        let flag = |name: &str| -> u64 {
            flags
                .lines()
                .find_map(|line| {
                    let mut fields = line.split_whitespace();
                    let _kind = fields.next()?;
                    (fields.next()? == name).then_some(())?;
                    (fields.next()? == "=").then_some(())?;
                    fields.next()?.parse().ok()
                })
                .unwrap_or_else(|| panic!("the JVM reports no {name}"))
        };
        assert_eq!(flag("TieredStopAtLevel"), 1);
        assert_eq!(flag("ReservedCodeCacheSize"), 256 * 1024 * 1024);
    }
}
