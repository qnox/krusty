//! What the native target promises about concurrency today, pinned so it cannot drift quietly.
//!
//! **Threads share one heap under one mutator lock.** A thread runs Kotlin because the runtime
//! started it (`kt_thread_start`: the kernel's `clone` on a stack the runtime maps, Go's way, with
//! no C library) or because foreign code did and called into Kotlin. Every such thread holds the
//! lock while it runs Kotlin and releases it while it is in foreign code, so at most one thread
//! runs Kotlin at a time and every hand-off of the lock orders memory. `docs/SPEC.md` records that
//! decision and what it leaves for later (safepoints, so that a thread looping in Kotlin cannot
//! keep the others waiting). `tests/native_runtime_e2e.rs` drives the lock with real threads.
//!
//! So these tests pin the contract as it stands: a construct whose meaning is exhausted by
//! one-thread-at-a-time execution compiles and runs, and a construct that needs real concurrency is
//! DECLINED rather than compiled into something that quietly ignores it.

use std::path::{Path, PathBuf};

use krusty::backend::{Artifact, Backend as _};
use krusty::diag::DiagSink;
use krusty::native::{CraneliftBackend, NativeTarget};
use krusty::source::SourceInput;

use super::common;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn host() -> Option<NativeTarget> {
    let target = NativeTarget::host()?;
    (krusty::native::can_link(target)
        && krusty::toolchain::stdlib_jar().is_some()
        && krusty::toolchain::jdk_modules().is_some())
    .then_some(target)
}

/// Compile one program with the native code generator for the host.
fn compile(source: &str) -> (Vec<Artifact>, Vec<String>) {
    let target = host().expect("checked by the caller");
    let classpath = std::rc::Rc::new(krusty::toolchain::stdlib_and_jdk_classpath());
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let inputs = vec![SourceInput::kotlin(source).with_file_stem("Main")];
    let mut diags = DiagSink::new();
    let mut features = krusty::features::LangFeatures::new();
    features.apply_source_directives(source);
    let backend = CraneliftBackend::new(target).verified();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        krusty::frontend::PlatformProvider::new(backend.compilation_target(), platform),
        &features,
        &mut diags,
    );
    let artifacts = krusty::compiler::emit_analyzed(
        analysis,
        &["Main".to_string()],
        &backend,
        "main",
        &mut diags,
    );
    (artifacts, diags.diags.into_iter().map(|d| d.msg).collect())
}

/// Compile, link and run; return standard output.
fn run(source: &str) -> String {
    let target = host().expect("checked by the caller");
    let (artifacts, diagnostics) = compile(source);
    assert!(
        diagnostics.is_empty(),
        "the program must compile: {diagnostics:?}"
    );
    let objects = artifacts
        .iter()
        .filter(|(name, _)| name.ends_with(".o"))
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();
    let image = krusty::native::link_program(&objects, target).expect("link");
    let scratch = Scratch::new("stress");
    let executable = scratch.path().join("program");
    std::fs::write(&executable, &image).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = common::run_freshly_written(std::process::Command::new(&executable).env_clear())
        .expect("run the built executable");
    assert!(
        output.status.success(),
        "the program must exit cleanly: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_volatile_property_compiles_and_reads_back_what_was_written() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `@Volatile` orders a field against OTHER THREADS. Only the holder of the mutator lock runs
    // Kotlin, and every hand-off of the lock is a release and an acquire, so every write before a
    // hand-off is visible after it: ordinary reads and writes already give the field the meaning
    // the annotation asks for. This test is to be re-read the day threads run Kotlin in parallel,
    // when the annotation acquires a meaning the generator does not yet implement.
    assert_eq!(
        run("@Volatile var flag: Boolean = false\n\
             @Volatile var counter: Int = 0\n\
             fun publish() { counter = 42; flag = true }\n\
             fun main() {\n\
             \x20   println(flag)\n\
             \x20   publish()\n\
             \x20   println(flag)\n\
             \x20   println(counter)\n\
             }\n"),
        "false\ntrue\n42\n"
    );
}

#[test]
fn a_suspend_function_that_never_suspends_is_an_ordinary_function() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // `suspend` is a calling convention, not concurrency: a suspend function that only computes
    // has nothing to suspend at, and lowering it as an ordinary function is exactly right. The
    // state machine a real suspension needs is a JVM pass, which is why the boundary below is
    // where it is.
    assert_eq!(
        run("suspend fun twice(n: Int): Int = n * 2\n\
             suspend fun sum(n: Int): Int = twice(n) + twice(n + 1)\n\
             fun start(): Int = 0\n\
             fun main() { println(start()) }\n"),
        "0\n"
    );
}

#[test]
fn a_real_suspension_is_declined_rather_than_dropped() {
    if host().is_none() {
        eprintln!("skipping: this build of krusty has no prebuilt native runtime for the host");
        return;
    }
    // The boundary that matters. A function that actually suspends needs a state machine to
    // resume into, and there is none here — so it must be REFUSED. Compiling it into a straight
    // call would produce a program that runs a coroutine's body to its first suspension and then
    // silently carries on, which is the failure this backend refuses to have.
    let (artifacts, diagnostics) = compile(
        "import kotlin.coroutines.intrinsics.*\n\
         suspend fun pause(): Int =\n\
         \x20   suspendCoroutineUninterceptedOrReturn { _ -> COROUTINE_SUSPENDED }\n\
         suspend fun outer(): Int = pause()\n\
         fun main() { println(\"unreachable\") }\n",
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("does not support")),
        "a real suspension must be declined, got {diagnostics:?}"
    );
    assert!(artifacts.is_empty(), "a declined file must emit no object");
}

#[test]
fn the_runtime_talks_to_the_kernel_directly() {
    // The runtime's whole kernel interface, read from the syscall header rather than trusted to a
    // comment: it starts threads with `clone`, waits and wakes on the mutator lock with `futex`,
    // and never goes through a C library to do either. A `pthread` here would make every static
    // program need a target libc, which is the toolchain the freestanding runtime exists to avoid.
    let syscalls = common::native_sys_header();
    assert!(
        !syscalls.contains("pthread"),
        "the runtime names `pthread`: a static program must not need a C library"
    );
    for expected in [
        "KT_SYS_WRITE",
        "KT_SYS_MMAP",
        "KT_SYS_MUNMAP",
        "KT_SYS_EXIT",
        "KT_SYS_EXIT_THREAD",
        "KT_SYS_CLONE",
        "KT_SYS_GETTID",
        "KT_SYS_FUTEX",
    ] {
        assert!(syscalls.contains(expected), "missing {expected}");
    }
}
