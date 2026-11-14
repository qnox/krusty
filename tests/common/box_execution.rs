//! Persistent JVM box execution and durable evidence for failed requests.

use super::{
    dir_classes, java_home, sweep_stale_temp_dirs, BoxRunner, ProfGuard, RunnerPool, BOX_RUNNER_SRC,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

/// Run `box()` on already-compiled classes via a persistent JVM keyed by `cp_jars` (the runtime
/// classpath — typically the stdlib jar so loaded classes resolve `kotlin.jvm.internal.*`). Returns
/// the `box()` return value (or `ERROR:…`), or `None` if the JVM environment is unavailable.
///
/// The runner is concurrency-safe (id-tagged requests, demuxed responses), so callers on different
/// threads share one JVM without an exclusive round-trip lock.
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
        record_failure(
            classes,
            box_class,
            cp_jars,
            &format!("JVM executable does not exist: {java}"),
        );
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
        Ok(s) => {
            if s.starts_with("ERROR:") {
                record_failure(classes, box_class, cp_jars, &s);
            }
            Some(s)
        }
        Err(error) => {
            record_failure(
                classes,
                box_class,
                cp_jars,
                &format!("original JVM request failed: {error}"),
            );
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
            match fresh.try_run(classes, box_class) {
                Ok(answer) => {
                    if answer.starts_with("ERROR:") {
                        record_failure(classes, box_class, cp_jars, &answer);
                    }
                    Some(answer)
                }
                Err(error) => {
                    record_failure(
                        classes,
                        box_class,
                        cp_jars,
                        &format!("replacement JVM request failed: {error}"),
                    );
                    None
                }
            }
        }
    }
}

fn record_failure(classes: &[(String, Vec<u8>)], main: &str, classpath: &[PathBuf], reason: &str) {
    let names = classes
        .iter()
        .map(|(name, _)| format!("{name}.class"))
        .collect::<Vec<_>>();
    let files = names
        .iter()
        .zip(classes)
        .map(|(name, (_, bytes))| (name.as_str(), bytes.as_slice()))
        .collect::<Vec<_>>();
    let report = format!("JVM box class: {main}; runtime classpath: {classpath:?}; {reason}");
    match crate::failure_evidence::retain(main, &report, &files) {
        Ok(path) => eprintln!("{report}; failure artifacts: {}", path.display()),
        Err(error) => eprintln!("{report}; could not preserve failure artifacts: {error}"),
    }
}

/// Compile `BoxRunner.java` once into a stable cache dir keyed by the source hash; return its dir.
fn setup_runner(java_home: &str) -> Option<PathBuf> {
    static SETUP: OnceLock<Option<PathBuf>> = OnceLock::new();
    SETUP
        .get_or_init(|| {
            let mut hash: u64 = 0xcbf29ce484222325;
            for b in BOX_RUNNER_SRC.bytes() {
                hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
            }
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("target/box_runner_{hash:016x}"));
            if dir.join("BoxRunner.class").is_file() {
                return Some(dir);
            }
            std::fs::create_dir_all(&dir).ok()?;
            let src_path = dir.join("BoxRunner.java");
            std::fs::write(&src_path, BOX_RUNNER_SRC).ok()?;
            let javac = format!("{java_home}/bin/javac");
            let out = Command::new(&javac)
                .args(["-source", "8", "-target", "8", "-d", dir.to_str().unwrap()])
                .arg(&src_path)
                .output();
            match out {
                Ok(out) if out.status.success() => Some(dir),
                result => {
                    let report = match result {
                        Ok(output) => format!(
                            "BoxRunner setup: {javac}; status {}; stdout {:?}; stderr {:?}",
                            output.status,
                            String::from_utf8_lossy(&output.stdout),
                            String::from_utf8_lossy(&output.stderr)
                        ),
                        Err(error) => {
                            format!("BoxRunner setup: {javac}; failed to launch: {error}")
                        }
                    };
                    let files = [("BoxRunner.java", BOX_RUNNER_SRC.as_bytes())];
                    match crate::failure_evidence::retain("jvm-runner-setup", &report, &files) {
                        Ok(path) => eprintln!("{report}; failure artifacts: {}", path.display()),
                        Err(error) => {
                            eprintln!("{report}; could not preserve failure artifacts: {error}")
                        }
                    }
                    None
                }
            }
        })
        .clone()
}
