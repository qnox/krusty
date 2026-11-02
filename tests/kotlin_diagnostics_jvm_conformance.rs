//! Kotlin's own diagnostics test data, `compiler/testData/diagnostics/{tests,testsWithStdLib}`,
//! scored by false accepts: programs the reference kotlinc rejects that krusty compiles without an
//! error. A false accept is the worst kind of divergence, since a build then ships classes for an
//! invalid program.
//!
//! A case is a `<name>.kt` file whose name has no other dot, the pattern Kotlin's own generated
//! diagnostics tests select (`.ll.kt`, `.latestLV.kt`, `.reversed.kt` and the other dotted files are
//! variants of a case, not cases). Its diagnostic markers (`<!NAME!>` … `<!>`) are stripped, and the
//! helper sources its directives ask for are added the way JetBrains' runner adds them
//! (`// CHECK_TYPE`, `// INFERENCE_HELPERS`). The case then compiles exactly as a box case does:
//! the same topology (`// FILE:` sets with Java, `// MODULE:` builds), the same directive-selected
//! command line, classpath and JDK, through the same krusty compile path and the same reference
//! kotlinc oracle. krusty runs in long-lived worker processes (`krusty_case_workers`), so a crash
//! or a compile past `KRUSTY_DIAGNOSTICS_CASE_TIMEOUT_SECONDS` fails that case, not the run. Only
//! the verdict is compared: whether each compiler reports an error. kotlinc is the oracle. Every case also states upstream whether it is expected to stop at the frontend
//! (`// RUN_PIPELINE_TILL: FRONTEND` or `FIR2IR`, that is, it has errors) or to reach the backend;
//! the summary counts the cases where kotlinc disagrees with that, which is where this runner does
//! not reproduce JetBrains' test configuration.
//!
//! A case kotlinc cannot finish (a crash, javac refusing the case's Java, a directive the oracle
//! does not model) is not scored, and only where `tests/diagnostics_not_scored/jvm/<version>.txt`
//! lists it: the run proves that list in both directions, so a case losing its oracle fails.
//!
//! False accepts ratchet: `tests/diagnostics_false_accepts/jvm/<version>.txt` lists the cases krusty
//! accepts although kotlinc rejects them. A case joining or leaving the list fails until the list is
//! updated (`KRUSTY_BLESS_DIAGNOSTICS_EXPECTATIONS=1` rewrites both lists from a full local run). A
//! listed case leaves only on a definite verdict from both compilers; blessing refuses while a listed
//! case has none. False rejects and krusty failures are counted and printed, not ratcheted.
//!
//! kotlinc's results are the box oracle's cached reference compiles, keyed by every input and the
//! exact compiler, so a run pays kotlinc only for cases whose inputs changed. A cold cache costs
//! about 1.5 hours of kotlinc compiles, so ordinary test runs ignore the test;
//! `just diagnostics-conformance` runs it explicitly under `KRUSTY_DIAGNOSTICS_TIMEOUT_SECONDS`, and
//! CI runs that for the newest supported Kotlin.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rayon::prelude::*;

use krusty::conformance::{BoxJdkRoots, TestTarget};
use krusty::kotlin_version::KotlinVersion;

use super::box_inventory;
use super::box_jvm_compile::{compile_box_case, CompileFailure, COROUTINE_HELPERS};
use super::box_ratchet;
use super::box_reference_classes::{self, strip_diagnostic_markers};
use super::common;
use super::krusty_case_workers::{serve, Reply, Workers};

/// The diagnostics directories, relative to the JetBrains checkout.
const CORPORA: [&str; 2] = [
    "compiler/testData/diagnostics/tests",
    "compiler/testData/diagnostics/testsWithStdLib",
];

/// The helper sources JetBrains' runner adds to every module of a case declaring the directive.
const HELPERS: [(&str, &str); 2] = [
    (
        "CHECK_TYPE",
        "compiler/testData/diagnostics/helpers/types/checkType.kt",
    ),
    (
        "INFERENCE_HELPERS",
        "compiler/testData/diagnostics/helpers/inference/inferenceUtils.kt",
    ),
];

const FALSE_ACCEPTS: &str = "diagnostics_false_accepts";
const NOT_SCORED: &str = "diagnostics_not_scored";

/// What a compiler did with a case.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    Accepted,
    /// Rejected, with the errors the compiler reported.
    Rejected(Vec<String>),
    /// Neither: the compilation could not be built or did not finish.
    Failed(String),
}

/// One case's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Agree,
    /// kotlinc rejects, krusty accepts: kotlinc's first error says what krusty missed.
    FalseAccept(String),
    /// kotlinc accepts, krusty rejects: krusty's first error.
    FalseReject(String),
    /// krusty neither accepted nor rejected.
    KrustyFailed(String),
    /// Not scored, for this reason.
    NotScored(String),
}

/// The case's source as both compilers receive it: markers stripped, the directive-selected helper
/// sources added to each module, and the JVM corpus placeholders applied.
fn prepare(root: &Path, source: &str, stem: &str) -> Result<String, String> {
    let mut helpers = Vec::new();
    for (directive, path) in HELPERS {
        if krusty::conformance::directive(source, directive) {
            let text = std::fs::read_to_string(root.join(path))
                .map_err(|error| format!("{directive} helper {path}: {error}"))?;
            let name = Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("helper.kt");
            helpers.push(format!("// FILE: {name}\n{text}"));
        }
    }
    let source = strip_diagnostic_markers(source);
    let source = if helpers.is_empty() {
        source
    } else {
        with_helper_files(&source, stem, &helpers.concat())
    };
    Ok(krusty::conformance::prepare_test_source(
        &source,
        TestTarget::Jvm,
    ))
}

/// `source` with `helpers` (`// FILE:` blocks) closing each of its modules. A case without
/// `// FILE:` markers becomes the single file its runner names after the case.
fn with_helper_files(source: &str, stem: &str, helpers: &str) -> String {
    let mut out = String::new();
    if !source.contains("// FILE:") && !source.contains("// MODULE:") {
        out.push_str(&format!("// FILE: {stem}.kt\n"));
    }
    let mut in_module = false;
    for line in source.split_inclusive('\n') {
        if line.starts_with("// MODULE:") {
            if in_module {
                out.push_str(helpers);
            }
            in_module = true;
        }
        out.push_str(line);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(helpers);
    out
}

/// kotlinc's verdict: the box oracle's reference compile of the same topology and arguments.
fn reference(
    source: &str,
    stem: &str,
    classpath: &[PathBuf],
    jdk: krusty::conformance::BoxJdk<'_>,
) -> Verdict {
    match box_reference_classes::reference_compile(source, stem, classpath, jdk, COROUTINE_HELPERS)
    {
        Ok(_) => Verdict::Accepted,
        Err(reason) if box_reference_classes::is_source_rejection(&reason) => {
            Verdict::Rejected(vec![reason])
        }
        Err(reason) => Verdict::Failed(reason),
    }
}

/// krusty's verdict, compiled exactly as the JVM box lane compiles a case. It runs in a worker
/// process ([`krusty_case_workers`]) so a compiler crash or a compile that never finishes costs
/// only this case.
fn krusty(
    source: &str,
    stem: &str,
    classpath: &[PathBuf],
    jdk: krusty::conformance::BoxJdk<'_>,
) -> Verdict {
    let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compile_box_case(source, stem, classpath, jdk.classpath_root(), &|_| {})
    }));
    match compiled {
        Ok(Ok(_)) => Verdict::Accepted,
        Ok(Err(CompileFailure::Rejected(errors))) => Verdict::Rejected(errors),
        Ok(Err(CompileFailure::Harness(reason))) => Verdict::Failed(reason),
        Err(panic) => Verdict::Failed(format!("panicked: {}", panic_message(&panic))),
    }
}

/// The variable that makes [`diagnostics_krusty_worker`] serve cases instead of returning.
const WORKER_ROLE: &str = "KRUSTY_DIAGNOSTICS_KRUSTY_WORKER";

/// How long a worker may take on one case before it is killed and the case fails.
fn case_deadline() -> Duration {
    let seconds = std::env::var("KRUSTY_DIAGNOSTICS_CASE_TIMEOUT_SECONDS")
        .ok()
        .map(|value| {
            value.parse::<u64>().unwrap_or_else(|_| {
                panic!("KRUSTY_DIAGNOSTICS_CASE_TIMEOUT_SECONDS must be whole seconds: {value}")
            })
        })
        .unwrap_or(60);
    Duration::from_secs(seconds)
}

/// krusty's verdict on the case at `file`, as a worker computes and encodes it.
fn worker_verdict(root: &Path, jdks: &BoxJdkRoots, file: &Path) -> String {
    let source = match std::fs::read_to_string(file) {
        Ok(source) => source,
        Err(error) => return format!("F\tread {}: {error}", file.display()),
    };
    let stem = case_stem(file);
    let prepared = match prepare(root, &source, stem) {
        Ok(prepared) => prepared,
        Err(reason) => return format!("F\t{reason}"),
    };
    let classpath = common::classpath_jars_for(&prepared);
    match krusty(&prepared, stem, &classpath, jdks.select(&prepared)) {
        Verdict::Accepted => "A".to_string(),
        Verdict::Rejected(errors) => format!("R\t{}", errors.first().cloned().unwrap_or_default()),
        Verdict::Failed(reason) => format!("F\t{reason}"),
    }
}

/// krusty's verdict on the case at `file`, from this thread's worker.
fn krusty_in_worker(workers: &Workers, file: &Path) -> Verdict {
    match workers.request(&file.display().to_string()) {
        Reply::Line(line) => match line.split_once('\t') {
            None if line == "A" => Verdict::Accepted,
            Some(("R", error)) => Verdict::Rejected(vec![error.to_string()]),
            Some(("F", reason)) => Verdict::Failed(reason.to_string()),
            _ => panic!("a worker replied {line:?}"),
        },
        Reply::TimedOut(deadline) => Verdict::Failed(format!(
            "did not finish within {}s (KRUSTY_DIAGNOSTICS_CASE_TIMEOUT_SECONDS)",
            deadline.as_secs()
        )),
        Reply::Died(status) => Verdict::Failed(format!("the compiler process died: {status}")),
    }
}

fn case_stem(file: &Path) -> &str {
    file.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("case")
}

/// The JDKs a case compiles against: the box corpus's mock runtime, or the full JDK on request.
fn box_jdks(root: &Path) -> BoxJdkRoots {
    let box_dir = root.join("compiler/testData/codegen/box");
    BoxJdkRoots::new(
        krusty::toolchain::box_mock_jdk_rt_jar(&box_dir).unwrap_or_else(|error| panic!("{error}")),
        Some(common::jdk_modules()),
    )
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|text| text.to_string()))
        .unwrap_or_else(|| "a non-text payload".to_string())
}

fn first_line(output: &str) -> &str {
    output
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
}

/// The case's upstream expectation: whether it stops with errors before the backend.
fn expects_errors(source: &str) -> Option<bool> {
    source.lines().find_map(|line| {
        match line.trim().strip_prefix("// RUN_PIPELINE_TILL:")?.trim() {
            "FRONTEND" | "FIR2IR" => Some(true),
            "BACKEND" => Some(false),
            _ => None,
        }
    })
}

struct Case {
    key: String,
    outcome: Outcome,
    /// Whether kotlinc's verdict disagrees with the case's `RUN_PIPELINE_TILL`.
    upstream_disagrees: bool,
    kotlinc_rejects: bool,
}

fn run_case(root: &Path, jdks: &BoxJdkRoots, workers: &Workers, file: &Path) -> Case {
    let key = box_inventory::key(root, file);
    let scored = |outcome, upstream_disagrees, kotlinc_rejects| Case {
        key: key.clone(),
        outcome,
        upstream_disagrees,
        kotlinc_rejects,
    };
    let source = std::fs::read_to_string(file)
        .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
    let stem = case_stem(file);
    let prepared = match prepare(root, &source, stem) {
        Ok(prepared) => prepared,
        Err(reason) => return scored(Outcome::NotScored(reason), false, false),
    };
    let classpath = common::classpath_jars_for(&prepared);
    let jdk = jdks.select(&prepared);
    let reference = reference(&prepared, stem, &classpath, jdk);
    let kotlinc_rejects = matches!(reference, Verdict::Rejected(_));
    let upstream_disagrees = matches!(
        (&reference, expects_errors(&source)),
        (Verdict::Accepted, Some(true)) | (Verdict::Rejected(_), Some(false))
    );
    let outcome = match (&reference, krusty_in_worker(workers, file)) {
        (Verdict::Failed(reason), _) => {
            Outcome::NotScored(format!("kotlinc failed: {}", first_line(reason)))
        }
        (_, Verdict::Failed(reason)) => Outcome::KrustyFailed(reason),
        (Verdict::Rejected(errors), Verdict::Accepted) => {
            Outcome::FalseAccept(errors.first().cloned().unwrap_or_default())
        }
        (Verdict::Accepted, Verdict::Rejected(errors)) => {
            Outcome::FalseReject(errors.first().cloned().unwrap_or_default())
        }
        _ => Outcome::Agree,
    };
    scored(outcome, upstream_disagrees, kotlinc_rejects)
}

/// Every case of the diagnostics corpora: `<name>.kt` files whose name has no other dot.
fn cases(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for corpus in CORPORA {
        let directory = root.join(corpus);
        assert!(
            directory.is_dir(),
            "{} is missing; re-provision the corpus with `just box-corpus <version>`",
            directory.display()
        );
        files.extend(
            krusty::conformance::kotlin_files(&directory)
                .into_iter()
                .filter(|file| {
                    file.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.matches('.').count() == 1)
                }),
        );
    }
    files.sort();
    files
}

const FALSE_ACCEPTS_HEADER: &str =
    "diagnostics test data (compiler/testData/diagnostics, relative to the\n\
     # checkout) that kotlinc rejects and krusty compiles without an error. The list only shrinks.";

const NOT_SCORED_HEADER: &str =
    "diagnostics test data that kotlinc gives no verdict on: it crashes, or\n\
     # javac refuses the case's Java. The reference run proves every entry.";

fn render_list(header: &str, version: KotlinVersion, keys: &BTreeSet<String>) -> String {
    let mut text = format!(
        "# Kotlin {version} {header}\n# Regenerate with KRUSTY_BLESS_DIAGNOSTICS_EXPECTATIONS=1 on a full local run.\n"
    );
    for key in keys {
        text.push_str(key);
        text.push('\n');
    }
    text
}

/// The keys an expectation list records for `version`, or none when no release records one.
fn read_list(directory: &str, version: KotlinVersion) -> BTreeSet<String> {
    let path = box_ratchet::read_path(directory, "jvm", version);
    match std::fs::read_to_string(&path) {
        Ok(text) => box_ratchet::parse(&text)
            .unwrap_or_else(|error| panic!("invalid expectation {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => panic!("read {}: {error}", path.display()),
    }
}

fn percent(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "n/a".to_string();
    }
    format!("{:.1}%", 100.0 * part as f64 / whole as f64)
}

/// A krusty worker of [`krusty_rejects_what_kotlinc_rejects_in_its_diagnostics_test_data`]: it
/// reads case paths from stdin and replies with krusty's verdicts. Without its role variable it
/// does nothing, so an ordinary ignored run passes it.
#[test]
#[ignore = "a krusty worker process of the diagnostics gate"]
fn diagnostics_krusty_worker() {
    let worker = std::thread::Builder::new()
        // The box lane's worker stack: resolution recurses deeply.
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            if std::env::var_os(WORKER_ROLE).is_none() {
                return;
            }
            let root = box_inventory::kotlin_checkout(CORPORA[0]);
            let jdks = box_jdks(&root);
            // A panic is that case's failure, replied with its message.
            std::panic::set_hook(Box::new(|_| {}));
            serve(WORKER_ROLE, |path| {
                worker_verdict(&root, &jdks, Path::new(path))
            });
        })
        .expect("start the worker thread");
    worker
        .join()
        .expect("the worker serves until its input closes");
}

#[test]
#[ignore = "compiles ten thousand cases with krusty and kotlinc; long on a cold kotlinc cache"]
fn krusty_rejects_what_kotlinc_rejects_in_its_diagnostics_test_data() {
    let root = box_inventory::kotlin_checkout(CORPORA[0]);
    let version = krusty::kotlin_version::target();
    let jdks = box_jdks(&root);
    let workers = Workers {
        test: "kotlin_diagnostics_jvm_conformance::diagnostics_krusty_worker",
        role: WORKER_ROLE,
        deadline: case_deadline(),
    };
    // Each pool thread owns one krusty worker process and waits on kotlinc's reference server.
    let cases: Vec<Case> = cases(&root)
        .par_iter()
        .map(|file| run_case(&root, &jdks, &workers, file))
        .collect();

    let mut false_accepts = BTreeMap::new();
    let mut false_rejects = 0;
    let mut krusty_failed = BTreeMap::<String, usize>::new();
    let mut not_scored = BTreeMap::<String, usize>::new();
    let mut rejected_by_kotlinc = 0;
    let mut upstream_disagrees = 0;
    for case in &cases {
        rejected_by_kotlinc += usize::from(case.kotlinc_rejects);
        upstream_disagrees += usize::from(case.upstream_disagrees);
        match &case.outcome {
            Outcome::Agree => {}
            Outcome::FalseAccept(error) => {
                false_accepts.insert(case.key.clone(), error.clone());
            }
            Outcome::FalseReject(_) => false_rejects += 1,
            Outcome::KrustyFailed(reason) => {
                *krusty_failed
                    .entry(first_line(reason).to_string())
                    .or_default() += 1
            }
            Outcome::NotScored(reason) => *not_scored.entry(reason.clone()).or_default() += 1,
        }
    }
    eprintln!(
        "diagnostics corpus ({version}): {} cases; kotlinc rejects {rejected_by_kotlinc}, krusty \
         accepts {} of them ({} false accepts); {false_rejects} false rejects; krusty failed on {}; \
         {upstream_disagrees} kotlinc verdicts disagree with RUN_PIPELINE_TILL",
        cases.len(),
        false_accepts.len(),
        percent(false_accepts.len(), rejected_by_kotlinc),
        krusty_failed.values().sum::<usize>(),
    );
    for (reason, count) in &krusty_failed {
        eprintln!("  krusty failed: {count} × {reason}");
    }
    for (reason, count) in &not_scored {
        eprintln!("  not scored: {count} × {reason}");
    }

    let found: BTreeSet<String> = false_accepts.keys().cloned().collect();
    let unscored: BTreeSet<String> = cases
        .iter()
        .filter(|case| matches!(case.outcome, Outcome::NotScored(_)))
        .map(|case| case.key.clone())
        .collect();
    let outcomes: BTreeMap<&String, &Outcome> = cases
        .iter()
        .map(|case| (&case.key, &case.outcome))
        .collect();
    let listed_false_accepts = read_list(FALSE_ACCEPTS, version);
    // A listed false accept leaves the list only on a definite observation: both compilers ran and
    // agreed. One whose case lost its verdict proves nothing, and blessing must not drop it.
    let mut problems: Vec<String> = listed_false_accepts
        .iter()
        .filter_map(|key| match outcomes.get(key) {
            Some(Outcome::NotScored(reason)) => Some(format!(
                "{key}: listed as a false accept, but kotlinc gave no verdict ({reason})"
            )),
            Some(Outcome::KrustyFailed(reason)) => Some(format!(
                "{key}: listed as a false accept, but krusty gave no verdict ({})",
                first_line(reason)
            )),
            Some(_) => None,
            None => Some(format!(
                "{key}: listed as a false accept but absent from the corpus"
            )),
        })
        .collect();
    let bless = box_ratchet::bless_requested(
        std::env::var("KRUSTY_BLESS_DIAGNOSTICS_EXPECTATIONS")
            .ok()
            .as_deref(),
    )
    .unwrap_or_else(|error| panic!("{}", error.replace("BOX", "DIAGNOSTICS")));
    if bless {
        assert!(
            std::env::var_os("CI").is_none(),
            "KRUSTY_BLESS_DIAGNOSTICS_EXPECTATIONS is refused under CI: expectations are blessed locally"
        );
        assert!(
            problems.is_empty(),
            "cannot bless while listed false accepts lack a verdict:\n  {}",
            problems.join("\n  ")
        );
        for (directory, keys, header) in [
            (FALSE_ACCEPTS, &found, FALSE_ACCEPTS_HEADER),
            (NOT_SCORED, &unscored, NOT_SCORED_HEADER),
        ] {
            box_ratchet::record(
                directory,
                "jvm",
                version,
                keys,
                &render_list(header, version, keys),
            )
            .unwrap_or_else(|error| panic!("record the {version} {directory}: {error}"));
        }
    }
    let expected = read_list(FALSE_ACCEPTS, version);
    for key in found.difference(&expected) {
        problems.push(format!(
            "{key}: krusty newly accepts it; kotlinc reports {}",
            false_accepts[key]
        ));
    }
    for key in expected.difference(&found) {
        if matches!(
            outcomes.get(key),
            Some(Outcome::Agree | Outcome::FalseReject(_))
        ) {
            problems.push(format!(
                "{key}: no longer a false accept; remove it from {}",
                box_ratchet::read_path(FALSE_ACCEPTS, "jvm", version).display()
            ));
        }
    }
    // A case kotlinc gives no verdict on is excused only where it is listed, so a case that loses its
    // oracle fails instead of quietly leaving the score.
    let expected_unscored = read_list(NOT_SCORED, version);
    for key in unscored.difference(&expected_unscored) {
        if let Some(Outcome::NotScored(reason)) = outcomes.get(key) {
            problems.push(format!("{key}: kotlinc gave no verdict ({reason})"));
        }
    }
    for key in expected_unscored.difference(&unscored) {
        problems.push(format!(
            "{key}: listed as not scored, but kotlinc gave a verdict; remove it from {}",
            box_ratchet::read_path(NOT_SCORED, "jvm", version).display()
        ));
    }
    assert!(
        rejected_by_kotlinc > 0,
        "kotlinc rejected no case: check the corpus and the reference compiler"
    );
    assert!(
        problems.is_empty(),
        "{} diagnostics case(s) disagree with the expectations:\n  {}",
        problems.len(),
        problems.join("\n  ")
    );
}
