//! The `codegen/box` corpus through krusty's native pipeline.
//!
//! Every case is a Kotlin file with `fun box(): String` that returns `"OK"` when the compiler got it
//! right. Here each single-file case is compiled by krusty's own code generator with a program entry
//! that prints `box()`'s result, linked by krusty's linker against the prebuilt runtime, and RUN;
//! the executable must print `OK` and exit cleanly.
//!
//! **Skipping is permitted; miscompiling is not.** The native generator covers a subset of Kotlin
//! and declines the rest by name, so a case it declines is a skip, counted by reason — the sorted
//! reasons are the backlog, printed with the report. A case the frontend rejects is a skip too
//! (the JVM gate measures that). But a case that compiles and links and then prints anything other
//! than `OK`, exits nonzero, or does not finish, is a FAILURE of this test: an accepted program must
//! run correctly, whatever fraction is accepted.
//!
//! Multi-file and multi-module cases (`// FILE:`, `// MODULE:`) are skipped as such for now: the
//! generator does not lower cross-file calls yet. The harness reads the corpus from
//! `KRUSTY_KOTLIN_BOX_DIR` (as the JVM gate does) and falls back to the vendored cases under
//! `tests/box_data/`, so it always runs against something.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::native::{CraneliftBackend, Entry, NativeTarget};
use krusty::source::SourceInput;

/// The prefix the native backend puts on every decline; what follows names the construct.
const DECLINE_PREFIX: &str = "krusty: the native backend does not support ";

/// How long one case may run. The programs are tiny; anything near this is a miscompiled loop.
const RUN_TIMEOUT: Duration = Duration::from_secs(10);

/// What happened to one case.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// `box()` printed `OK`.
    Pass,
    /// The native backend declined a construct, named.
    Declined(String),
    /// The frontend rejected the source (not this backend's business), carrying what it said.
    ///
    /// The reason is kept for the same purpose a decline's is: this bucket is a thousand cases
    /// wide and opaque without it, and what is in it is core work that every backend would gain
    /// from — so it is ranked in the report rather than left as a number.
    Frontend(String),
    /// The compiler panicked somewhere other than the native backend: no program was produced, so
    /// nothing was miscompiled, but a compiler defect is a compiler defect and it is listed.
    FrontendPanic(String),
    /// A directive puts the case outside this harness: multi-file, another backend, and so on.
    NotApplicable(&'static str),
    /// Accepted, but the program did not run correctly — the one outcome that fails the test.
    Failed(String),
}

fn host() -> Option<NativeTarget> {
    let target = NativeTarget::host()?;
    (krusty::native::can_link(target)
        && krusty::toolchain::stdlib_jar().is_some()
        && krusty::toolchain::jdk_modules().is_some())
    .then_some(target)
}

/// The corpus: the reference checkout when provisioned, else the vendored cases.
fn corpus_dir() -> PathBuf {
    krusty::toolchain::box_corpus_dir()
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/box_data"))
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-native-box-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Why a case is outside this harness before any compilation, or `None` when it applies.
fn not_applicable(src: &str) -> Option<&'static str> {
    if src.contains("// MODULE:") {
        return Some("multi-module case");
    }
    if src.contains("// FILE:") {
        return Some("multi-file case");
    }
    // The corpus's JVM ignores count here too, and that is not laziness. krusty has ONE frontend
    // and ONE common lowering; the native backend is a third consumer of the same checked IR the
    // JVM backend consumes. A case the corpus mutes on `JVM_IR` is muted because that semantics is
    // not reachable through this frontend at all — `null as T` for an erased `T`, say — so running
    // it natively measures the frontend, which the JVM lane already measures, and its verdict there
    // is the same wrong answer. When the native target grows its own frontend semantics (klib
    // ingestion, phase 7), this widening is what should narrow back to `NATIVE` alone.
    if !krusty::conformance::backend_applicable(src, &["NATIVE", "JVM", "JVM_IR"]) {
        return Some("not targeted at the native backend or muted on JVM");
    }
    if krusty::conformance::needs_unmodeled_compiler_flag(src) {
        return Some("needs a compiler flag krusty does not model");
    }
    for directive in [
        "WITH_COROUTINES",
        "WITH_REFLECT",
        "FULL_JDK",
        "JVM_TARGET",
        "JVM_ABI_K1_K2_DIFF",
        "TARGET_PLATFORM",
    ] {
        if krusty::conformance::directive(src, directive) {
            return Some("platform-specific directive");
        }
    }
    None
}

thread_local! {
    /// One classpath per worker thread: it is `Rc`-shared and caches parsed class files, so
    /// building it per case would redo the stdlib jar's index thousands of times.
    ///
    /// The stdlib jar AND the JDK jimage, which is the pair the JVM lane compiles against. Nothing
    /// about the emitted program changes — it links against the runtime and no JVM is involved —
    /// but the SIGNATURES come out of JVM artifacts until the provider is klib-based (see
    /// `native::intrinsics`), and a mapped builtin like `kotlin.Throwable` is a typealias for a
    /// `java.lang` class. Without the jimage those names resolve to nothing, and a case that fails
    /// to RESOLVE is counted as declined — so the lane was reporting "not supported yet" for a
    /// whole family of programs it had never actually attempted. The two lanes have to compile
    /// against the same thing for the comparison between them to mean anything.
    static CLASSPATH: std::rc::Rc<Classpath> = std::rc::Rc::new(Classpath::new(
        [
            krusty::toolchain::stdlib_jar().expect("checked by `host`"),
            krusty::toolchain::jdk_modules().expect("checked by `host`"),
        ]
        .into_iter()
        // `kotlin-test`, which is what `// WITH_STDLIB` means on top of the stdlib: the corpus
        // checks itself with `assertEquals`, `assertTrue` and `assertFailsWith`, and without the
        // jar those do not resolve — nor does the `kotlin.test` package path itself, which is why
        // `test` was the second most unresolved name in the whole corpus. The JVM lane has carried
        // it all along; this one had not.
        .chain(krusty::toolchain::kotlin_test_jar())
        .collect::<Vec<_>>(),
    ));
}

thread_local! {
    /// The file the most recent panic on this thread was raised in, recorded by the panic hook so
    /// a caught panic can be attributed to the native backend or to the rest of the compiler.
    static LAST_PANIC_FILE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Cases known to run wrong today, with the reason each is a KNOWN defect rather than a new one:
/// `<path relative to the corpus root><TAB><reason>`. Every failure not on this list fails the
/// test; so does a listed case that passes, so the list can only shrink honestly. These are
/// frontend and common-lowering defects the JVM lane fails on too — the native lane inherits
/// them and must not hide them.
const EXPECTED_FAILURES: &str = include_str!("native_box_expected_failures.txt");

fn expected_failures() -> BTreeMap<&'static str, &'static str> {
    EXPECTED_FAILURES
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('\t'))
        .map(|(path, reason)| (path.trim(), reason.trim()))
        .collect()
}

/// Whether this run reads its symbols from the Kotlin/Native KLIB instead of the JVM jar.
///
/// Opt-in (`KRUSTY_NATIVE_KLIB=1`), because it is a MEASUREMENT of how far the klib provider has
/// come and not yet the gate: the two providers spell an owner differently — the jar says
/// `kotlin/collections/CollectionsKt`, the klib says `kotlin/collections` — and `intrinsics` is
/// keyed on the jar's spelling, so a klib run declines where a jar run lowers. Running the corpus
/// both ways is how that distance stops being a guess.
fn reads_the_klib() -> bool {
    std::env::var("KRUSTY_NATIVE_KLIB").is_ok_and(|value| !value.is_empty() && value != "0")
}

thread_local! {
    /// The klib-backed provider, read once per worker thread: decoding and indexing the whole
    /// stdlib per case would redo it thousands of times, exactly as the classpath beside it would.
    /// The handle shares its tables, so the clone each case takes is a refcount.
    static KLIB: Option<krusty::native::libraries::NativeLibraries> =
        krusty::toolchain::kotlin_native_root().map(|root| {
            krusty::native::libraries::NativeLibraries::from_distribution(&root)
                .expect("the Kotlin/Native stdlib klib loads")
        });
}

fn klib_libraries() -> Option<krusty::native::libraries::NativeLibraries> {
    KLIB.with(Clone::clone)
}

/// Compile one case with the native backend; the object, or why not.
fn compile(source: &str, stem: &str, target: NativeTarget) -> Result<Vec<u8>, Outcome> {
    let classpath = CLASSPATH.with(std::rc::Rc::clone);
    let platform: Box<dyn krusty::libraries::SemanticPlatform> = match reads_the_klib() {
        true => Box::new(klib_libraries().expect("checked by `host`")),
        false => Box::new(
            krusty::jvm::jvm_libraries::JvmLibraries::new(classpath.clone())
                .expect("JVM provider initialization"),
        ),
    };
    let prepared = krusty::conformance::prepare_test_source(source);
    let inputs = vec![SourceInput::kotlin(&prepared).with_file_stem(stem)];
    let stems = vec![stem.to_string()];
    let mut features = krusty::features::LangFeatures::new();
    features.apply_source_directives(&prepared);
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    if let Some(first) = diags.diags.first() {
        return Err(Outcome::Frontend(first.msg.clone()));
    }
    // The backend asks the PROVIDER what it realized an identity as, not a classpath. This is a
    // second view over the very same `Rc<Classpath>` the frontend's provider wrapped: the interned
    // identity tables live in the classpath, so both views answer from one set of records.
    let provider: std::rc::Rc<dyn krusty::libraries::SemanticPlatform> = match reads_the_klib() {
        true => std::rc::Rc::new(klib_libraries().expect("checked by `host`")),
        false => std::rc::Rc::new(
            krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
                .expect("JVM provider initialization"),
        ),
    };
    let backend = CraneliftBackend::new(provider, target).with_entry(Entry::Box);
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "box", &mut diags);
    if let Some(decline) = diags
        .diags
        .iter()
        .find_map(|diagnostic| diagnostic.msg.strip_prefix(DECLINE_PREFIX))
    {
        return Err(Outcome::Declined(
            decline.trim_end_matches(" yet").to_string(),
        ));
    }
    if let Some(first) = diags.diags.first() {
        return Err(Outcome::Frontend(first.msg.clone()));
    }
    match artifacts.into_iter().next() {
        Some((_, object)) => Ok(object),
        // The frontend produced no checked file for the backend to lower and said nothing about
        // it. That is the JVM pipeline's silence to account for, not a native miscompile.
        None => Err(Outcome::Frontend(
            "the frontend produced no checked file and said nothing".to_string(),
        )),
    }
}

/// Run the linked program with a deadline; its stdout, or what went wrong.
fn run(executable: &Path) -> Result<String, String> {
    let mut child = super::common::spawn_freshly_written(
        Command::new(executable)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .map_err(|error| format!("could not start the program: {error}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > RUN_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "the program did not finish within {}s",
                    RUN_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
            Err(error) => return Err(format!("waiting for the program: {error}")),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("collecting output: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        return Err(format!(
            "exit {}; stdout {stdout:?}; stderr {:?}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(stdout)
}

fn outcome(scratch: &Path, file: &Path, target: NativeTarget) -> Outcome {
    let Ok(source) = std::fs::read_to_string(file) else {
        return Outcome::NotApplicable("unreadable");
    };
    if let Some(reason) = not_applicable(&source) {
        return Outcome::NotApplicable(reason);
    }
    let stem = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Case");
    let object = match compile(&source, stem, target) {
        Ok(object) => object,
        Err(outcome) => return outcome,
    };
    let image = match krusty::native::link_program(&[&object], target) {
        Ok(image) => image,
        Err(error) => return Outcome::Failed(format!("link: {error}")),
    };
    let executable = scratch.join(format!(
        "{stem}-{}-{}",
        std::process::id(),
        rayon::current_thread_index().unwrap_or(0)
    ));
    if let Err(error) = std::fs::write(&executable, &image) {
        return Outcome::Failed(format!("writing the executable: {error}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755));
    }
    let result = run(&executable);
    let _ = std::fs::remove_file(&executable);
    match result {
        // The entry prints what `box()` returned, as the LAST line. Anything before it is the
        // PROGRAM's own output — `controlStructures/kt513.kt` builds a list and `println`s it
        // before returning `OK` — which the JVM lane never sees, because there the answer is a
        // return value rather than a stream. Requiring the whole of stdout to be `OK` counted
        // every such case as a miscompile, which is the opposite of what it is.
        //
        // This does not weaken the check: a program that prints `OK` itself and then answers
        // something else still fails, because the answer is what comes last.
        Ok(stdout) if stdout.trim_end_matches('\n').rsplit('\n').next() == Some("OK") => {
            Outcome::Pass
        }
        Ok(stdout) => Outcome::Failed(format!("box() printed {stdout:?}")),
        Err(error) => Outcome::Failed(error),
    }
}

/// The report line, in a stable machine-readable shape:
/// `native conformance: scanned S, passed P, declined D, frontend F, not-applicable N,
/// frontend-panics Q, known-failures K, failed X`.
fn report_line(counts: &[(&str, usize)]) -> String {
    let fields = counts
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("native conformance: {fields}")
}

#[test]
fn report_line_has_a_stable_shape() {
    assert_eq!(
        report_line(&[("scanned", 3), ("passed", 1)]),
        "native conformance: scanned 3, passed 1"
    );
}

#[test]
fn kotlin_codegen_box_native_conformance() {
    let Some(target) = host() else {
        // Written to the report too, so a CI lane that skipped says so in its artifact instead
        // of reading as a run that found nothing.
        let notice = "native conformance: skipped (no prebuilt native runtime for this host)";
        eprintln!("{notice}");
        if let Ok(path) = std::env::var("KRUSTY_NATIVE_CONFORMANCE_REPORT") {
            let _ = std::fs::write(path, format!("{notice}\n"));
        }
        return;
    };
    let corpus = corpus_dir();
    let files = krusty::conformance::kotlin_files(&corpus);
    assert!(
        !files.is_empty(),
        "no Kotlin sources under {}",
        corpus.display()
    );
    // Triage knob: scan only the cases whose path contains this substring. The expected-failure
    // checks below cover the cases actually scanned, so a narrowed run stays self-consistent.
    let files = match std::env::var("KRUSTY_NATIVE_BOX_ONLY") {
        Ok(pattern) => files
            .into_iter()
            .filter(|file| file.to_string_lossy().contains(&pattern))
            .collect(),
        Err(_) => files,
    };
    let limit = std::env::var("KRUSTY_NATIVE_BOX_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);
    let files = krusty::conformance::evenly_sample(files, limit);
    // One PARTITION of the corpus, so a lane that outgrew the two-minute process deadline is
    // divided rather than exempted — `scripts/test-gate-defaults.sh` states that as the rule, and
    // the JVM lane is partitioned the same way. Every count this test reports, the expected-failure
    // ledger included, is over the cases actually scanned, so a shard stays self-consistent: a
    // ledger entry in another shard is simply not scanned here, and one in THIS shard that passes
    // still fails the run.
    //
    // Striding by position rather than splitting into blocks keeps the shards comparable: the
    // corpus is ordered by path, so contiguous blocks would hand one shard a whole directory of
    // near-identical cases and another the long tail.
    let files = match (
        std::env::var("KRUSTY_NATIVE_CONFORMANCE_SHARD_INDEX"),
        std::env::var("KRUSTY_NATIVE_CONFORMANCE_SHARD_COUNT"),
    ) {
        (Ok(index), Ok(count)) => {
            let index: usize = index
                .parse()
                .expect("KRUSTY_NATIVE_CONFORMANCE_SHARD_INDEX must be an integer");
            let count: usize = count
                .parse()
                .expect("KRUSTY_NATIVE_CONFORMANCE_SHARD_COUNT must be an integer");
            assert!(
                count > 0,
                "KRUSTY_NATIVE_CONFORMANCE_SHARD_COUNT must be positive"
            );
            assert!(
                index < count,
                "KRUSTY_NATIVE_CONFORMANCE_SHARD_INDEX {index} is outside 0..{count}"
            );
            files
                .into_iter()
                .enumerate()
                .filter_map(|(position, file)| (position % count == index).then_some(file))
                .collect()
        }
        (Err(_), Err(_)) => files,
        _ => panic!(
            "KRUSTY_NATIVE_CONFORMANCE_SHARD_INDEX and KRUSTY_NATIVE_CONFORMANCE_SHARD_COUNT \
             must be set together"
        ),
    };

    let scratch = Scratch::new();
    // Panics are recorded per case below; the default hook's backtrace spam would bury the
    // report. The hook keeps the panic's file so the case can be attributed.
    std::panic::set_hook(Box::new(|info| {
        let file = info.location().map(|location| location.file().to_string());
        LAST_PANIC_FILE.with(|last| *last.borrow_mut() = file);
    }));
    let known = expected_failures();

    // The same worker pool shape as the JVM gate: frontend analysis dominates each case, and the
    // deep recursion of resolution wants a wide stack.
    let mut pool = rayon::ThreadPoolBuilder::new().stack_size(64 * 1024 * 1024);
    if let Some(threads) = std::env::var("KRUSTY_TEST_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        pool = pool.num_threads(threads);
    }
    let pool = pool.build().expect("build the worker pool");

    let started = Instant::now();
    let outcomes: Vec<(&PathBuf, Outcome)> = pool.install(|| {
        files
            .par_iter()
            .map(|file| {
                // A panic inside the compiler is a failure of THAT case, recorded like any other
                // so one bug does not hide the picture for the rest of the corpus; the test still
                // fails.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    outcome(&scratch.0, file, target)
                }))
                .unwrap_or_else(|panic| {
                    let message = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_else(|| "panic".to_string());
                    let origin = LAST_PANIC_FILE.with(|last| last.borrow_mut().take());
                    match origin {
                        Some(origin) if !origin.contains("src/native/") => {
                            Outcome::FrontendPanic(format!("{message} ({origin})"))
                        }
                        _ => Outcome::Failed(format!("compiler panic: {message}")),
                    }
                });
                (file, result)
            })
            .collect()
    });

    let mut passed = 0usize;
    let mut frontend = 0usize;
    let mut declined: BTreeMap<String, usize> = BTreeMap::new();
    let mut frontend_reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut not_applicable: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut traced: Vec<(PathBuf, String)> = Vec::new();
    let mut frontend_panics: Vec<(PathBuf, String)> = Vec::new();
    let mut known_failures: Vec<(PathBuf, String)> = Vec::new();
    let mut failed: Vec<(PathBuf, String)> = Vec::new();
    for (file, result) in outcomes {
        let relative = file
            .strip_prefix(&corpus)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned();
        match result {
            Outcome::Pass if known.contains_key(relative.as_str()) => failed.push((
                file.clone(),
                "listed in native_box_expected_failures.txt but passes now: remove it".to_string(),
            )),
            Outcome::Pass => passed += 1,
            Outcome::Frontend(reason) => {
                frontend += 1;
                // Traced beside the declines, because this is now the larger backlog: a ranked
                // reason with no case behind it cannot be reproduced, and an "internal error"
                // line is unfollowable without the source that raised it.
                traced.push((file.clone(), reason.clone()));
                // Kept VERBATIM, unlike a decline. Almost every one of these is an unresolved
                // reference, and there the name is the whole of the information: the shape says
                // only "something was not found", while the name says which symbol the provider
                // does not expose and therefore what to fix.
                *frontend_reasons.entry(reason).or_default() += 1;
            }
            Outcome::FrontendPanic(reason) => frontend_panics.push((file.clone(), reason)),
            Outcome::Declined(reason) => {
                traced.push((file.clone(), reason.clone()));
                *declined.entry(reason).or_default() += 1;
            }
            Outcome::NotApplicable(reason) => *not_applicable.entry(reason).or_default() += 1,
            Outcome::Failed(reason) => match known.get(relative.as_str()) {
                Some(why) => known_failures.push((file.clone(), format!("{reason} — {why}"))),
                None => failed.push((file.clone(), reason)),
            },
        }
    }
    failed.sort();
    known_failures.sort();
    frontend_panics.sort();

    let declined_total: usize = declined.values().sum();
    let not_applicable_total: usize = not_applicable.values().sum();
    let report = report_line(&[
        ("scanned", files.len()),
        ("passed", passed),
        ("declined", declined_total),
        ("frontend", frontend),
        ("not-applicable", not_applicable_total),
        ("frontend-panics", frontend_panics.len()),
        ("known-failures", known_failures.len()),
        ("failed", failed.len()),
    ]);
    eprintln!("{report} ({:.1}s)", started.elapsed().as_secs_f64());

    // Triage: name the cases behind one decline reason, so a line in the table below can be
    // followed back to the source that produced it.
    if let Ok(pattern) = std::env::var("KRUSTY_NATIVE_BOX_TRACE") {
        for (file, reason) in &traced {
            if reason.contains(&pattern) {
                eprintln!("  traced {}: {reason}", file.display());
            }
        }
    }

    // The other backlog, and the larger one: what the FRONTEND refuses. Every case here is core
    // work — nothing about it is this backend's — and all three backends would gain from it, which
    // is why it is ranked beside the declines rather than left as a single number.
    let mut frontend_ranked: Vec<(&String, &usize)> = frontend_reasons.iter().collect();
    frontend_ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (reason, count) in frontend_ranked.iter().take(60) {
        eprintln!("  frontend {count:>5}  {reason}");
    }

    // The backlog: what the generator declines, most frequent first.
    let mut reasons: Vec<(&String, &usize)> = declined.iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (reason, count) in reasons.iter().take(40) {
        eprintln!("  declined {count:>5}  {reason}");
    }
    for (reason, count) in &not_applicable {
        eprintln!("  not applicable {count:>5}  {reason}");
    }
    if let Ok(path) = std::env::var("KRUSTY_NATIVE_CONFORMANCE_REPORT") {
        if let Ok(mut out) = std::fs::File::create(path) {
            let _ = writeln!(out, "{report}");
            for (reason, count) in &reasons {
                let _ = writeln!(out, "declined\t{count}\t{reason}");
            }
            for (reason, count) in &frontend_ranked {
                let _ = writeln!(out, "frontend\t{count}\t{reason}");
            }
        }
    }

    for (file, reason) in &frontend_panics {
        eprintln!("  frontend panic {}: {reason}", file.display());
    }
    for (file, reason) in &known_failures {
        eprintln!("  known failure {}: {reason}", file.display());
    }
    let _ = std::panic::take_hook();
    for (file, reason) in &failed {
        eprintln!("FAILED {}: {reason}", file.display());
    }
    assert!(
        failed.is_empty(),
        "{} accepted case(s) did not run correctly — a native program the generator accepted must \
         print OK; skipping is permitted, miscompiling is not",
        failed.len()
    );
}
