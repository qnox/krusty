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
//!   KRUSTY_KOTLIN_STDLIB    kotlin-stdlib.jar (added to runner classpath)
//!   KRUSTY_BOX_LIMIT        cap on files scanned (default: all)

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

use krusty::codegen::emit::{emit_class, emit_file, file_class_name};
use krusty::diag::DiagSink;
use krusty::jvm::classreader::parse_class;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

// BoxRunner.java source embedded at compile time; compiled once at test start.
const BOX_RUNNER_SRC: &str = r#"
import java.io.*;
import java.util.concurrent.*;

public class BoxRunner {
    static final long TIMEOUT_MS = 5000; // 5s per test
    static final ExecutorService EXEC = Executors.newCachedThreadPool(r -> {
        Thread t = new Thread(r);
        t.setDaemon(true);
        return t;
    });

    public static void main(String[] args) throws Exception {
        DataInputStream din = new DataInputStream(new BufferedInputStream(System.in, 65536));
        DataOutputStream dout = new DataOutputStream(new BufferedOutputStream(System.out, 4096));
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
                    return "ERROR:" + t.getClass().getSimpleName() + ":" + t.getMessage();
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
            } else if p.extension().map_or(false, |e| e == "kt") {
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
fn compile_source(src: &str, stem: &str) -> Option<Vec<(String, Vec<u8>)>> {
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
    let syms = collect_signatures(&files, &mut diags);
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

    let mut outputs: Vec<(String, Vec<u8>)> = Vec::new();
    let facade_name = file_class_name(stem, file.package.as_deref());

    let t4 = std::time::Instant::now();
    // Emit declared classes.
    for &d in &file.decls {
        if let krusty::ast::Decl::Class(c) = file.decl(d) {
            let internal = match file.package.as_deref() {
                Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), c.name),
                _ => c.name.clone(),
            };
            let (bytes, extra) = emit_class(c, &file, &info, &internal, &facade_name, &syms, &mut diags);
            if diags.has_errors() {
                T_EMIT.fetch_add(t4.elapsed().as_nanos() as u64, Ordering::Relaxed);
                return None;
            }
            outputs.push((internal, bytes));
            outputs.extend(extra);
        }
    }

    // Emit file facade for top-level functions/properties.
    let has_facade = file.decls.iter().any(|&d| {
        matches!(file.decl(d), krusty::ast::Decl::Fun(_) | krusty::ast::Decl::Property(_))
    });
    if has_facade {
        let internal = file_class_name(stem, file.package.as_deref());
        let (bytes, extra) = emit_file(&file, &info, &syms, &internal, &mut diags);
        if diags.has_errors() {
            T_EMIT.fetch_add(t4.elapsed().as_nanos() as u64, Ordering::Relaxed);
            return None;
        }
        outputs.push((internal, bytes));
        outputs.extend(extra);
    }
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
            if ci.method("box", "()Ljava/lang/String;").map_or(false, |m| m.is_static()) {
                return Some(name.replace('/', "."));
            }
        }
    }
    None
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
        BoxRunner { _child: child, stdin, stdout }
    }

    /// Send class bytes and box class name; return the result string.
    fn run(&mut self, classes: &[(String, Vec<u8>)], box_class: &str) -> String {
        // Write: [u32 n][for each: u16 name_len, name, u32 data_len, data][u16 box_len, box_name]
        let n = classes.len() as u32;
        self.stdin.write_all(&n.to_be_bytes()).unwrap();
        for (name, data) in classes {
            let nl = name.len() as u16;
            self.stdin.write_all(&nl.to_be_bytes()).unwrap();
            self.stdin.write_all(name.as_bytes()).unwrap();
            let dl = data.len() as u32;
            self.stdin.write_all(&dl.to_be_bytes()).unwrap();
            self.stdin.write_all(data).unwrap();
        }
        let bl = box_class.len() as u16;
        self.stdin.write_all(&bl.to_be_bytes()).unwrap();
        self.stdin.write_all(box_class.as_bytes()).unwrap();
        self.stdin.flush().unwrap();

        // Read: [u32 result_len][result_bytes]
        let mut len_buf = [0u8; 4];
        self.stdout.read_exact(&mut len_buf).unwrap();
        let rlen = u32::from_be_bytes(len_buf) as usize;
        let mut result = vec![0u8; rlen];
        self.stdout.read_exact(&mut result).unwrap();
        String::from_utf8_lossy(&result).into_owned()
    }
}

/// Compile and run BoxRunner.java once, return path to the directory with BoxRunner.class.
fn setup_runner(java_home: &str, work: &Path) -> PathBuf {
    let runner_dir = work.join("runner");
    fs::create_dir_all(&runner_dir).unwrap();
    let src_path = runner_dir.join("BoxRunner.java");
    fs::write(&src_path, BOX_RUNNER_SRC).unwrap();
    let javac = format!("{java_home}/bin/javac");
    let out = Command::new(&javac)
        .args(["-source", "8", "-target", "8", "-d", runner_dir.to_str().unwrap()])
        .arg(&src_path)
        .output()
        .expect("javac failed to launch");
    if !out.status.success() {
        panic!("BoxRunner.java compile failed:\n{}", String::from_utf8_lossy(&out.stderr));
    }
    runner_dir
}

#[test]
fn kotlin_codegen_box_conformance() {
    let Some(box_dir) = env("KRUSTY_KOTLIN_BOX_DIR") else {
        eprintln!("skipping box conformance: set KRUSTY_KOTLIN_BOX_DIR");
        return;
    };
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping box conformance: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let stdlib = env("KRUSTY_KOTLIN_STDLIB").unwrap_or_default();
    let limit: usize = env("KRUSTY_BOX_LIMIT").and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);

    let mut files = Vec::new();
    collect_kt(Path::new(&box_dir), &mut files);
    files.truncate(limit);

    let work = std::env::temp_dir().join(format!("krusty_box_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    // Compile BoxRunner.java once.
    let runner_cp = setup_runner(&java_home, &work);
    let runner_cp_str = runner_cp.to_str().unwrap().to_string();

    // One Mutex<BoxRunner> per rayon thread — lazy init on first use.
    // We use a global pool indexed by rayon thread index.
    let n_threads = rayon::current_num_threads();
    let runners: Vec<Mutex<Option<BoxRunner>>> = (0..n_threads)
        .map(|_| Mutex::new(None))
        .collect();

    // Phase timers (nanoseconds, accumulated across threads).
    let t_compile = AtomicU64::new(0);
    let t_jvm = AtomicU64::new(0);
    let t_total_start = std::time::Instant::now();

    // Parallel phase: compile each test in-process, run in the per-thread JVM.
    let results: Vec<(PathBuf, TestResult)> = files
        .par_iter()
        .map(|file| {
            let src = fs::read_to_string(file).unwrap_or_default();
            // Skip multi-file, multi-module, or no-box tests.
            if src.contains("// FILE:") || src.contains("// MODULE:") || !src.contains("fun box()") {
                return (file.clone(), TestResult::Skip);
            }

            // In-process compilation.
            let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("File").to_string();
            let t0 = std::time::Instant::now();
            let classes = match compile_source(&src, &stem) {
                Some(c) => c,
                None => return (file.clone(), TestResult::Skip),
            };
            t_compile.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
            let box_class = match find_box_class(&classes) {
                Some(c) => c,
                None => return (file.clone(), TestResult::Skip),
            };

            // Execute in the per-thread persistent JVM.
            let tid = rayon::current_thread_index().unwrap_or(0);
            let mut guard = runners[tid].lock().unwrap();
            if guard.is_none() {
                *guard = Some(BoxRunner::new(&java, &runner_cp_str, &stdlib));
            }
            let runner = guard.as_mut().unwrap();
            let t1 = std::time::Instant::now();
            let result = runner.run(&classes, &box_class);
            t_jvm.fetch_add(t1.elapsed().as_nanos() as u64, Ordering::Relaxed);

            if result == "OK" {
                (file.clone(), TestResult::Pass)
            } else {
                (file.clone(), TestResult::Fail(result))
            }
        })
        .collect();

    let total_ms = t_total_start.elapsed().as_millis();
    let compile_ms = t_compile.load(Ordering::Relaxed) / 1_000_000;
    let jvm_ms = t_jvm.load(Ordering::Relaxed) / 1_000_000;
    let lex_ms = T_LEX.load(Ordering::Relaxed) / 1_000_000;
    let parse_ms = T_PARSE.load(Ordering::Relaxed) / 1_000_000;
    let sigs_ms = T_SIGS.load(Ordering::Relaxed) / 1_000_000;
    let check_ms = T_CHECK.load(Ordering::Relaxed) / 1_000_000;
    let emit_ms = T_EMIT.load(Ordering::Relaxed) / 1_000_000;
    eprintln!("timing (wall={total_ms}ms, thread-sum): compile={compile_ms}ms [lex={lex_ms}ms parse={parse_ms}ms sigs={sigs_ms}ms check={check_ms}ms emit={emit_ms}ms]  jvm={jvm_ms}ms");

    let _ = fs::remove_dir_all(&work);

    let mut compiled = 0usize;
    let mut passed = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (file, r) in &results {
        match r {
            TestResult::Skip => skipped += 1,
            TestResult::Pass => { compiled += 1; passed += 1; }
            TestResult::Fail(why) => {
                compiled += 1;
                failures.push(format!("{}: {why}", file.display()));
            }
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
    assert!(failures.is_empty(), "{} box case(s) miscompiled (see above)", failures.len());
    assert!(passed > 0, "no box() cases ran — check KRUSTY_KOTLIN_BOX_DIR / JDK");
}

enum TestResult {
    Skip,
    Pass,
    Fail(String),
}
