//! Kotlin's own diagnostics test data, `compiler/testData/diagnostics/{tests,testsWithStdLib}`,
//! scored by false accepts: programs the reference kotlinc rejects that krusty compiles without an
//! error. A false accept is the worst kind of divergence, since a build then ships classes for an
//! invalid program.
//!
//! A case is a `<name>.kt` file whose name has no other dot, the pattern Kotlin's own generated
//! diagnostics tests select (`.ll.kt`, `.latestLV.kt`, `.reversed.kt` and the other dotted files are
//! variants of a case, not cases). Its diagnostic markers (`<!NAME!>` … `<!>`) are stripped, its
//! `// FILE:` blocks become the sources of one compilation, Java blocks included, and its
//! `// LANGUAGE:` directives become `-XXLanguage:` arguments. kotlinc and krusty then compile exactly
//! the same sources with exactly the same arguments, and their exit codes are compared: 0 accepts,
//! 1 rejects. kotlinc is the oracle. Every case also states upstream whether it is expected to stop
//! at the frontend (`// RUN_PIPELINE_TILL: FRONTEND` or `FIR2IR`, that is, it has errors) or to reach
//! the backend; the summary counts the cases where plain kotlinc disagrees with that, which is
//! where this runner does not reproduce JetBrains' test configuration.
//!
//! A `// MODULE:` case needs a compilation per module and is not compiled yet: it is counted, by
//! that reason, and never scored. A case kotlinc does not finish with 0 or 1 is not scored either.
//!
//! False accepts ratchet: `tests/diagnostics_false_accepts/jvm/<version>.txt` lists the cases krusty
//! accepts although kotlinc rejects them. A case joining or leaving the list fails until the list is
//! updated (`KRUSTY_BLESS_DIAGNOSTICS_EXPECTATIONS=1` rewrites it from a full local run). False
//! rejects and krusty crashes are counted and printed, not ratcheted.
//!
//! kotlinc's exit code and errors for a case are cached in `target/cache/diagnostics-reference`,
//! keyed by the case's sources and arguments and the exact compiler identity, so a run pays only for
//! cases whose inputs changed.
//!
//! A full run compiles every case with krusty, which takes tens of minutes, and on a cold cache with
//! kotlinc too, which takes hours. Ordinary test runs therefore ignore it; `just
//! diagnostics-conformance` runs it explicitly under `KRUSTY_DIAGNOSTICS_TIMEOUT_SECONDS`, and CI
//! runs that for the newest supported Kotlin.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use krusty::kotlin_version::KotlinVersion;

use super::box_inventory;
use super::box_lane::Scratch;
use super::box_ratchet;
use super::box_reference_classes::strip_diagnostic_markers;
use super::common::{self, byte_dump, language_directives};

/// The diagnostics directories, relative to the JetBrains checkout.
const CORPORA: [&str; 2] = [
    "compiler/testData/diagnostics/tests",
    "compiler/testData/diagnostics/testsWithStdLib",
];

/// Bumped when what a cache entry records, or how it is keyed, changes.
const REFERENCE_SALT: &str = "diagnostics-reference-v1";

const FALSE_ACCEPTS: &str = "diagnostics_false_accepts";

/// What a compiler did with a case.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    Accepted,
    /// Rejected, with the errors the compiler reported.
    Rejected(Vec<String>),
    /// Neither: an internal error, a crash, or no exit code at all.
    Failed(String),
}

impl Verdict {
    fn of(code: Option<i32>, output: &str) -> Verdict {
        match code {
            Some(0) => Verdict::Accepted,
            Some(1) => Verdict::Rejected(
                output
                    .lines()
                    .filter(|line| line.contains("error: "))
                    .map(str::to_string)
                    .collect(),
            ),
            Some(code) => Verdict::Failed(format!("exit code {code}: {}", first_line(output))),
            None => Verdict::Failed("killed by a signal".to_string()),
        }
    }
}

fn first_line(output: &str) -> &str {
    output
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
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

/// A case prepared for both compilers: its sources, written under its scratch directory, and the
/// arguments both compile it with besides `-d`.
struct Prepared {
    sources: Vec<(PathBuf, String)>,
    arguments: Vec<String>,
}

fn prepare(source: &str, stem: &str, scratch: &Path) -> Result<Prepared, String> {
    if source.contains("// MODULE:") {
        return Err("// MODULE: cases need a compilation per module".to_string());
    }
    let source = strip_diagnostic_markers(source);
    let (kotlin, java) = krusty::conformance::split_files(&source);
    let mut sources = Vec::new();
    if kotlin.is_empty() && java.is_empty() {
        sources.push((
            scratch.join("src/0").join(format!("{stem}.kt")),
            source.clone(),
        ));
    }
    // Each block in a directory of its own: blocks may share a leaf name, and the file name is
    // part of what is compiled (a facade class is named after it).
    for (index, (name, text)) in kotlin.into_iter().enumerate() {
        sources.push((scratch.join(format!("src/{index}/{name}.kt")), text));
    }
    for (index, (name, text)) in java.into_iter().enumerate() {
        sources.push((scratch.join(format!("java/{index}/{name}")), text));
    }
    Ok(Prepared {
        sources,
        arguments: language_directives::kotlinc_args(&source),
    })
}

fn command_line(prepared: &Prepared, output: &Path) -> Vec<String> {
    let mut arguments = vec!["-d".to_string(), output.display().to_string()];
    arguments.extend(prepared.arguments.iter().cloned());
    arguments.extend(
        prepared
            .sources
            .iter()
            .map(|(path, _)| path.display().to_string()),
    );
    arguments
}

/// kotlinc's verdict, from the cache or the persistent reference compiler.
fn reference(key: &str, prepared: &Prepared, scratch: &Path) -> Result<Verdict, String> {
    let relative = |path: &Path| {
        path.strip_prefix(scratch)
            .unwrap_or(path)
            .display()
            .to_string()
    };
    let mut parts: Vec<Vec<u8>> = vec![REFERENCE_SALT.into(), key.into()];
    for (path, text) in &prepared.sources {
        parts.push(relative(path).into_bytes());
        parts.push(text.clone().into_bytes());
    }
    parts.extend(
        prepared
            .arguments
            .iter()
            .map(|argument| argument.clone().into_bytes()),
    );
    let parts: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    let fingerprint = byte_dump::fingerprint_parts(&parts);
    // A snapshot, dev or beta compiler is not an immutable build: its verdicts are never cached.
    let compiler = byte_dump::published_compiler_id();
    let entry = compiler.as_ref().map(|compiler| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/cache/diagnostics-reference")
            .join(compiler)
            .join(format!("{fingerprint:032x}"))
    });
    if let Some(cached) = entry.as_deref().and_then(load_verdict) {
        return Ok(cached);
    }
    let output = scratch.join("kotlinc-out");
    let (code, stderr) =
        common::kotlinc_server::kotlinc_compile_unrecorded(&command_line(prepared, &output))
            .ok_or("the reference kotlinc is unavailable")?;
    let scratch_text = format!("{}/", scratch.display());
    let verdict = Verdict::of(Some(code), &stderr.replace(&scratch_text, ""));
    // Only a definite verdict is cached: a failure may be transient.
    if let Some(entry) = entry.filter(|_| !matches!(verdict, Verdict::Failed(_))) {
        store_verdict(&entry, &verdict);
    }
    Ok(verdict)
}

/// A cache entry: `accepted`, or `rejected` followed by one error per line.
fn load_verdict(entry: &Path) -> Option<Verdict> {
    let text = std::fs::read_to_string(entry).ok()?;
    let mut lines = text.lines();
    match lines.next()? {
        "accepted" => Some(Verdict::Accepted),
        "rejected" => Some(Verdict::Rejected(lines.map(str::to_string).collect())),
        _ => None,
    }
}

fn store_verdict(entry: &Path, verdict: &Verdict) {
    let text = match verdict {
        Verdict::Accepted => "accepted\n".to_string(),
        Verdict::Rejected(errors) => format!(
            "rejected\n{}",
            errors.iter().map(|e| format!("{e}\n")).collect::<String>()
        ),
        Verdict::Failed(_) => return,
    };
    let written = entry
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| box_ratchet::write_atomic(entry, &text));
    if let Err(error) = written {
        panic!(
            "cache the reference verdict at {}: {error}",
            entry.display()
        );
    }
}

/// How long krusty may take on one case; a case that runs longer is a krusty failure, so one
/// non-terminating compilation cannot stall the lane. `KRUSTY_DIAGNOSTICS_CASE_TIMEOUT_SECONDS`
/// raises it on slow machines.
fn case_timeout() -> Duration {
    let seconds = std::env::var("KRUSTY_DIAGNOSTICS_CASE_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(60);
    Duration::from_secs(seconds)
}

fn krusty(prepared: &Prepared, scratch: &Path) -> Verdict {
    let output = scratch.join("krusty-out");
    // Redirect to files, not pipes: a child that fills a pipe blocks before `try_wait` sees it exit.
    let stderr_path = scratch.join("krusty.stderr");
    let stderr = match std::fs::File::create(&stderr_path) {
        Ok(file) => file,
        Err(error) => return Verdict::Failed(format!("cannot capture krusty's errors: {error}")),
    };
    let child = Command::new(common::krusty_binary())
        .args(command_line(prepared, &output))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => return Verdict::Failed(format!("cannot run krusty: {error}")),
    };
    let timeout = case_timeout();
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Verdict::Failed(format!("did not finish within {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Verdict::Failed(format!("waiting for krusty: {error}"));
            }
        }
    };
    let stderr = std::fs::read(&stderr_path).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr).replace(&format!("{}/", scratch.display()), "");
    Verdict::of(status.code(), &stderr)
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

fn run_case(root: &Path, file: &Path) -> Case {
    let key = box_inventory::key(root, file);
    let scored = |outcome, upstream_disagrees, kotlinc_rejects| Case {
        key: key.clone(),
        outcome,
        upstream_disagrees,
        kotlinc_rejects,
    };
    let source = std::fs::read_to_string(file)
        .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
    let scratch = Scratch::new("diagnostics");
    let scratch = scratch.0.as_path();
    let stem = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("case");
    let prepared = match prepare(&source, stem, scratch) {
        Ok(prepared) => prepared,
        Err(reason) => return scored(Outcome::NotScored(reason), false, false),
    };
    for (path, text) in &prepared.sources {
        std::fs::create_dir_all(path.parent().expect("a source has a directory"))
            .and_then(|()| std::fs::write(path, text))
            .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    }
    let reference = match reference(&key, &prepared, scratch) {
        Ok(verdict) => verdict,
        Err(reason) => panic!("{key}: {reason}"),
    };
    let kotlinc_rejects = matches!(reference, Verdict::Rejected(_));
    let upstream_disagrees = matches!(
        (&reference, expects_errors(&source)),
        (Verdict::Accepted, Some(true)) | (Verdict::Rejected(_), Some(false))
    );
    let outcome = match (&reference, krusty(&prepared, scratch)) {
        (Verdict::Failed(reason), _) => Outcome::NotScored(format!("kotlinc failed: {reason}")),
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

fn render_false_accepts(version: KotlinVersion, keys: &BTreeSet<String>) -> String {
    let mut text = format!(
        "# Kotlin {version} diagnostics test data (compiler/testData/diagnostics, relative to the\n\
         # checkout) that kotlinc rejects and krusty compiles without an error. Regenerate with\n\
         # KRUSTY_BLESS_DIAGNOSTICS_EXPECTATIONS=1 on a full local run. The list only shrinks.\n"
    );
    for key in keys {
        text.push_str(key);
        text.push('\n');
    }
    text
}

fn percent(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "n/a".to_string();
    }
    format!("{:.1}%", 100.0 * part as f64 / whole as f64)
}

#[test]
#[ignore = "compiles ten thousand cases with krusty, and with kotlinc on a cold cache (hours)"]
fn krusty_rejects_what_kotlinc_rejects_in_its_diagnostics_test_data() {
    let root = box_inventory::kotlin_checkout(CORPORA[0]);
    let version = krusty::kotlin_version::target();
    let cases: Vec<Case> = cases(&root)
        .par_iter()
        .map(|file| run_case(&root, file))
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
                if reason.starts_with("did not finish") {
                    eprintln!("  krusty {reason}: {}", case.key);
                }
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
    for (reason, count) in &not_scored {
        eprintln!("  not scored: {count} × {reason}");
    }

    let found: BTreeSet<String> = false_accepts.keys().cloned().collect();
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
        box_ratchet::record(
            FALSE_ACCEPTS,
            "jvm",
            version,
            &found,
            &render_false_accepts(version, &found),
        )
        .unwrap_or_else(|error| panic!("record the {version} false accepts: {error}"));
    }
    let path = box_ratchet::read_path(FALSE_ACCEPTS, "jvm", version);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let expected = box_ratchet::parse(&text)
        .unwrap_or_else(|error| panic!("invalid expectation {}: {error}", path.display()));
    let keys: BTreeSet<&String> = cases.iter().map(|case| &case.key).collect();
    let mut problems = Vec::new();
    for key in found.difference(&expected) {
        problems.push(format!(
            "{key}: krusty newly accepts it; kotlinc reports {}",
            false_accepts[key]
        ));
    }
    for key in expected.difference(&found) {
        if keys.contains(key) {
            problems.push(format!(
                "{key}: no longer a false accept; remove it from {}",
                path.display()
            ));
        } else {
            problems.push(format!("{key}: listed but absent from the corpus"));
        }
    }
    assert!(
        rejected_by_kotlinc > 0,
        "kotlinc rejected no case: check the corpus and the reference compiler"
    );
    assert!(
        problems.is_empty(),
        "{} diagnostics case(s) disagree with {}:\n  {}",
        problems.len(),
        path.display(),
        problems.join("\n  ")
    );
}
