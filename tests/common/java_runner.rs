//! The persistent javac/javap runner JVMs, one pool per JDK.

use super::*;

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
            String driver, cp, outdir, mainClass, procPath, extraArgs;
            try { driver = readStr(din); } catch (EOFException e) { break; }
            cp = readStr(din); outdir = readStr(din); mainClass = readStr(din);
            procPath = readStr(din);
            extraArgs = readStr(din);
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
                // Extra javac options, one per line (`--enable-preview`, `--release N`).
                if (!extraArgs.isEmpty()) for (String o : extraArgs.split("\n")) jargs.add(o);
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
    for b in JAVA_RUNNER_SRC
        .bytes()
        .chain(producing_jdk::driver_cache_key(java_home))
    {
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
        // UTF-8 explicitly: before JDK 18 the default charset follows the locale.
        .args(["-encoding", "UTF-8", "-d", &dir.to_string_lossy()])
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
        cmd.args(protocol_jvm_log_args("javarunner"));
        // Cap heap for footprint, but keep the default collector + full tiered JIT: JavaRunner executes
        // the reference javac output on a thread pool, so serial GC / C1-only throttles it (see BoxRunner).
        cmd.args([
            "-Xverify:all",
            "-Dfile.encoding=UTF-8",
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
        extra_args: &str,
    ) -> std::io::Result<String> {
        self.write_str(driver)?;
        self.write_str(cp)?;
        self.write_str(outdir)?;
        self.write_str(main_class)?;
        self.write_str(proc_path)?;
        self.write_str(extra_args)?;
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
// `common` is also compiled by the conformance test crate, which does not exercise JVM drivers.
#[allow(dead_code)]
pub fn javac_run(driver_path: &str, cp: &str, outdir: &str, main_class: &str) -> Option<String> {
    javac_run_proc(driver_path, cp, outdir, main_class, "")
}

/// [`javac_run`] with an annotation `-processorpath` (empty = no processing) — javac runs its own
/// multi-round APT loop in the same persistent JVM.
// `common` is also compiled by the conformance test crate, which does not exercise JVM drivers.
#[allow(dead_code)]
pub fn javac_run_proc(
    driver_path: &str,
    cp: &str,
    outdir: &str,
    main_class: &str,
    proc_path: &str,
) -> Option<String> {
    javac_run_on(
        &JavacTarget::default(),
        driver_path,
        cp,
        outdir,
        main_class,
        proc_path,
    )
}

/// The JDK a Java compilation runs on. A `boxModernJdk` case's Java sources compile on the JDK the
/// case runs on: javac's default class-file version is that JDK's, and only that javac reads the
/// case's preview-marked Kotlin classes.
#[derive(Clone, Debug, Default)]
pub struct JavacTarget {
    /// The JDK installation (`None` = the harness JDK).
    pub home: Option<PathBuf>,
    /// The feature release, named to javac next to `--enable-preview`.
    pub release: Option<u32>,
    pub preview: bool,
}

impl JavacTarget {
    fn javac_options(&self) -> String {
        match (self.preview, self.release) {
            (true, Some(release)) => format!("--enable-preview\n--release\n{release}"),
            (true, None) => "--enable-preview".to_string(),
            (false, _) => String::new(),
        }
    }
}

/// [`javac_run_proc`] on the JDK `target` names.
pub fn javac_run_on(
    target: &JavacTarget,
    driver_path: &str,
    cp: &str,
    outdir: &str,
    main_class: &str,
    proc_path: &str,
) -> Option<String> {
    // A pool of runner JVMs per JDK. The claim is held across the run, so overlapping javac/javap
    // calls use distinct JVMs instead of queueing on the first one.
    static POOLS: Mutex<
        std::collections::BTreeMap<String, &'static server_pool::Pool<JavaRunner>>,
    > = Mutex::new(std::collections::BTreeMap::new());
    let java_home = match &target.home {
        Some(home) => home.to_string_lossy().into_owned(),
        None => java_home(),
    };
    let java = format!("{java_home}/bin/java");
    if !Path::new(&java).exists() {
        return None;
    }
    let runner_dir = setup_java_runner(&java_home)?;
    let pool: &'static server_pool::Pool<JavaRunner> = {
        let mut pools = POOLS.lock().unwrap();
        let runners = pools
            .entry(java_home.clone())
            .or_insert_with(|| Box::leak(Box::new(server_pool::Pool::new())));
        runners
    };
    let options = target.javac_options();
    pool.with_server(
        server_pool::server_pool_cap(),
        || JavaRunner::new(&java, &runner_dir),
        |runner| match runner.try_run(driver_path, cp, outdir, main_class, proc_path, &options) {
            Ok(output) => Some(output),
            Err(_) => {
                *runner = JavaRunner::new(&java, &runner_dir)?;
                runner
                    .try_run(driver_path, cp, outdir, main_class, proc_path, &options)
                    .ok()
            }
        },
    )
    .flatten()
}
