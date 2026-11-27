//! Kotlin's diagnostics test data, `compiler/testData/diagnostics/{tests,testsWithStdLib}`, scored by
//! the diagnostics themselves: whether krusty reports exactly the errors and warnings kotlinc
//! reports, under kotlinc's factory names, over the same source ranges, with the factory's severity.
//!
//! JetBrains' runner checks every diagnostic kotlinc reports on a case against the case's markers
//! (`<!NAME!>` … `<!>`, [`diagnostic_markers`]), so the markers are kotlinc's ledger and this lane
//! needs no reference compile. As kotlinc's runner does, a ledger leaves out what the case's
//! `// DIAGNOSTICS:` directive disables (`-NAME`, or `-warnings` for every warning) and every
//! diagnostic the command line never prints: a name the release does not declare (the runner's
//! own `DEBUG_INFO_*`) and an `INFO` factory.
//!
//! A case compiles exactly as the false-accept lane ([`kotlin_diagnostics_jvm_conformance`])
//! compiles it, in the same long-lived worker processes, and krusty's report is read through
//! [`recording_diagnostics`]. The harness lays every file it compiles over the prepared case line
//! for line (a `// FILE:` section or `// MODULE:` unit keeps its lines' numbers), and the prepared
//! case knows the case line each of its lines is ([`PreparedCase`]), so a diagnostic lands at its
//! position in the case by its file's position, never by searching for the file's text. Like
//! JetBrains' runner, the lane checks only the case's own files, not the helpers a directive adds,
//! and leaves out `UNDERSCORE_USAGE_WITHOUT_BACKTICKS` where `// CHECK_TYPE` adds its helper, whose
//! `checkType { _<T>() }` idiom kotlinc's command line reports.
//!
//! The cases whose ledgers differ ratchet in `tests/diagnostics_ledger_mismatches/jvm/<version>.txt`:
//! a case joining or leaving the list fails until the list is updated, and
//! `KRUSTY_BLESS_DIAGNOSTICS_EXPECTATIONS=1` rewrites it from a full local run. A case krusty gives
//! no ledger on (a crash, a timeout, a topology krusty's compile path cannot build) does not report
//! kotlinc's diagnostics, so it is a mismatch and must be listed. A case the harness cannot observe
//! at all (its source or helpers unreadable) fails the run whatever the list says, and refuses
//! blessing. The run prints the diagnostic totals and the names krusty misses or adds most, the
//! measure the list's size alone does not give.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rayon::prelude::*;

use krusty::conformance::BoxJdkRoots;
use krusty::diag::names::DiagnosticSeverity;
use krusty::diag::{DiagnosticName, Severity};
use krusty::kotlin_version::KotlinVersion;

use super::box_inventory;
use super::box_jvm_compile::{
    compile_box_case, recording_diagnostics, CompileFailure, ReportedFile,
};
use super::box_ratchet;
use super::common;
use super::diagnostic_markers;
use super::kotlin_diagnostics_jvm_conformance::{
    box_jdks, case_deadline, case_stem, cases, first_line, panic_message, percent, prepare,
    LineOrigin, PreparedCase, CORPORA,
};
use super::krusty_case_workers::{serve, Reply, Workers};

const MISMATCHES: &str = "diagnostics_ledger_mismatches";

const MISMATCHES_HEADER: &str =
    "diagnostics test data (compiler/testData/diagnostics, relative to the\n\
     # checkout) whose diagnostics krusty does not report exactly as kotlinc does. The list only shrinks.";

/// The variable that makes [`diagnostics_ledger_worker`] serve cases instead of returning.
const WORKER_ROLE: &str = "KRUSTY_DIAGNOSTICS_LEDGER_WORKER";

/// One diagnostic of a ledger: its kotlinc name (or what krusty reported instead of one) and its
/// range in the case's marker-free text (start and end offsets), if it lies in that text.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    name: String,
    range: Option<(usize, usize)>,
}

/// What a case's `// DIAGNOSTICS:` directives disable.
#[derive(Default)]
struct Disabled {
    names: BTreeSet<String>,
    warnings: bool,
}

impl Disabled {
    fn of(source: &str) -> Disabled {
        let mut disabled = Disabled::default();
        for line in source.lines() {
            let Some(list) = line.trim().strip_prefix("// DIAGNOSTICS:") else {
                continue;
            };
            for token in list
                .split([' ', ',', '\t'])
                .filter(|token| !token.is_empty())
            {
                match token.strip_prefix('-') {
                    Some("warnings") => disabled.warnings = true,
                    Some(name) => {
                        disabled.names.insert(name.to_string());
                    }
                    None => {}
                }
            }
        }
        // The runner's `checkType { _<T>() }` idiom calls the helper's `_`, which kotlinc's command
        // line reports; JetBrains' runner leaves that diagnostic out of every case using it.
        if krusty::conformance::directive(source, "CHECK_TYPE") {
            disabled
                .names
                .insert("UNDERSCORE_USAGE_WITHOUT_BACKTICKS".to_string());
        }
        disabled
    }

    fn excludes(&self, name: &str, warning: bool) -> bool {
        self.names.contains(name) || (warning && self.warnings)
    }
}

/// The severity the command line prints a factory with, or `None` when it prints none.
fn printed_severity(name: DiagnosticName, version: KotlinVersion) -> Option<Severity> {
    match name.severity(version)? {
        DiagnosticSeverity::Error => Some(Severity::Error),
        DiagnosticSeverity::Warning | DiagnosticSeverity::StrongWarning => Some(Severity::Warning),
        DiagnosticSeverity::Info => None,
    }
}

/// kotlinc's ledger of a case: its markers, without what the command line never prints.
fn expected(source: &str, version: KotlinVersion) -> Vec<Entry> {
    let disabled = Disabled::of(source);
    let mut entries: Vec<Entry> = diagnostic_markers::parse(source)
        .markers
        .into_iter()
        .filter(|marker| {
            let Some(severity) = DiagnosticName::parse(&marker.name)
                .and_then(|name| printed_severity(name, version))
            else {
                return false;
            };
            !disabled.excludes(&marker.name, severity == Severity::Warning)
        })
        .map(|marker| Entry {
            name: marker.name,
            range: Some((marker.range.start, marker.range.end)),
        })
        .collect();
    entries.sort();
    entries
}

/// krusty's ledger of a case whose marker-free text is `text` and whose prepared source is
/// `prepared`, from the files its compilations reported in. A named diagnostic reported with a
/// severity other than its factory's keeps the difference in its entry's name, as does a diagnostic
/// krusty does not name.
fn reported(
    text: &str,
    prepared: &PreparedCase,
    files: &[ReportedFile],
    disabled: &Disabled,
    version: KotlinVersion,
) -> Vec<Entry> {
    let case_lines = line_starts(text);
    let prepared_lines: Vec<&str> = prepared.text.lines().collect();
    let mut entries = Vec::new();
    for file in files {
        let lines: Vec<&str> = file.text.lines().collect();
        // The harness lays each file it splits out of the prepared case over it line for line,
        // padding the lines that belong to other files with blank ones. A file not laid over it is
        // one the harness generated (`// WITH_COROUTINES` helpers), never the case's own.
        let laid_over = lines.len() <= prepared_lines.len()
            && lines
                .iter()
                .zip(&prepared_lines)
                .all(|(line, prepared)| line.is_empty() || line == prepared);
        // A helper file a directive adds: JetBrains' runner checks only the case's own files.
        let own = lines.iter().enumerate().any(|(index, line)| {
            !line.is_empty()
                && !matches!(prepared.origins.get(index), None | Some(LineOrigin::Added))
        });
        if !laid_over || !own {
            continue;
        }
        let file_lines = line_starts(&file.text);
        // The case offset of `offset` in this file: the same column of the case line its file line
        // is. `Err(true)` for a line the preparation added, `Err(false)` for one it rewrote or a
        // blank line standing for another file's.
        let case_offset = |offset: usize| -> Result<usize, bool> {
            let line = file_lines.partition_point(|&start| start <= offset) - 1;
            let column = offset - file_lines[line];
            let blank_for_another = lines.get(line).is_some_and(|text| text.is_empty())
                && prepared_lines
                    .get(line)
                    .is_some_and(|text| !text.is_empty());
            match prepared.origins.get(line) {
                _ if blank_for_another => Err(false),
                Some(LineOrigin::Case(case)) => case_lines
                    .get(*case)
                    .map(|start| start + column)
                    .ok_or(false),
                Some(LineOrigin::Rewritten(_)) => Err(false),
                Some(LineOrigin::Added) => Err(true),
                // The end of a file that ends with its last line's newline.
                None if line == prepared.origins.len() => Ok(text.len()),
                None => Err(false),
            }
        };
        for diagnostic in &file.diagnostics {
            let warning = diagnostic.severity == Severity::Warning;
            let name = match diagnostic.name {
                Some(name) => {
                    let spelling = name.as_str();
                    if disabled.excludes(spelling, warning) {
                        continue;
                    }
                    match printed_severity(name, version) {
                        None => continue,
                        Some(severity) if severity == diagnostic.severity => spelling.to_string(),
                        Some(_) => format!("{spelling} as {}", severity_word(diagnostic.severity)),
                    }
                }
                None if warning && disabled.warnings => continue,
                None => format!("<unnamed {}>", severity_word(diagnostic.severity)),
            };
            let lo = case_offset(diagnostic.span.lo as usize);
            // A diagnostic in text the preparation added (a directory default, an appended helper)
            // is the helper's, which the runner does not check either.
            if lo == Err(true) {
                continue;
            }
            // One in a line the preparation rewrote has no range in the case, and never matches
            // a marker.
            let range = lo.ok().zip(case_offset(diagnostic.span.hi as usize).ok());
            entries.push(Entry { name, range });
        }
    }
    entries.sort();
    entries
}

/// The offset each line of `text` starts at, and the end of a text that ends with a newline.
fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect()
}

fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    }
}

/// The entries of `left` that `right` does not match, as multisets.
fn difference(left: &[Entry], right: &[Entry]) -> Vec<Entry> {
    let mut unmatched: BTreeMap<&Entry, usize> = BTreeMap::new();
    for entry in right {
        *unmatched.entry(entry).or_default() += 1;
    }
    left.iter()
        .filter(|entry| match unmatched.get_mut(entry) {
            Some(count) if *count > 0 => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .cloned()
        .collect()
}

/// krusty's ledger of the case at `file`, as a worker computes and encodes it: `L` and one
/// `name\x1elo\x1ehi` record per entry, separated by `\x1f`; `F` and why krusty gave none; or `H`
/// and why the harness could not observe the case.
fn worker_ledger(root: &Path, jdks: &BoxJdkRoots, file: &Path) -> String {
    let source = match std::fs::read_to_string(file) {
        Ok(source) => source,
        Err(error) => return format!("H\tread {}: {error}", file.display()),
    };
    let stem = case_stem(file);
    let prepared = match prepare(root, file, &source, stem) {
        Ok(prepared) => prepared,
        Err(reason) => return format!("H\tprepare: {reason}"),
    };
    let classpath = common::classpath_jars_for(&prepared.text);
    let jdk = jdks.select(&prepared.text);
    let (compiled, files) = recording_diagnostics(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compile_box_case(
                &prepared.text,
                stem,
                &classpath,
                jdk.classpath_root(),
                &|_| {},
            )
        }))
    });
    match compiled {
        Ok(Ok(_) | Err(CompileFailure::Rejected { .. })) => {}
        Ok(Err(CompileFailure::Harness(reason))) => return format!("F\t{reason}"),
        Err(panic) => return format!("F\tpanicked: {}", panic_message(&panic)),
    }
    let text = diagnostic_markers::parse(&source).text;
    let entries = reported(
        &text,
        &prepared,
        &files,
        &Disabled::of(&source),
        krusty::kotlin_version::environment_release(),
    );
    let records: Vec<String> = entries
        .iter()
        .map(|entry| match &entry.range {
            Some((lo, hi)) => format!("{}\x1e{lo}\x1e{hi}", entry.name),
            None => format!("{}\x1e\x1e", entry.name),
        })
        .collect();
    format!("L\t{}", records.join("\x1f"))
}

/// What a worker observed on a case.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Observed {
    /// krusty's ledger.
    Ledger(Vec<Entry>),
    /// krusty gave no ledger (a crash, a timeout, a topology its compile path cannot build): why.
    NoLedger(String),
    /// The harness could not observe the case at all: why.
    Unobservable(String),
}

fn decode(line: &str) -> Observed {
    match line.split_once('\t') {
        Some(("L", "")) => Observed::Ledger(Vec::new()),
        Some(("L", records)) => Observed::Ledger(
            records
                .split('\x1f')
                .map(|record| {
                    let mut fields = record.split('\x1e');
                    let name = fields.next().unwrap_or_default().to_string();
                    let lo = fields.next().and_then(|lo| lo.parse().ok());
                    let hi = fields.next().and_then(|hi| hi.parse().ok());
                    Entry {
                        name,
                        range: lo.zip(hi),
                    }
                })
                .collect(),
        ),
        Some(("F", reason)) => Observed::NoLedger(reason.to_string()),
        Some(("H", reason)) => Observed::Unobservable(reason.to_string()),
        _ => panic!("a ledger worker replied {line:?}"),
    }
}

fn krusty_in_worker(workers: &Workers, file: &Path) -> Observed {
    match workers.request(&file.display().to_string()) {
        Reply::Line(line) => decode(&line),
        Reply::TimedOut(deadline) => Observed::NoLedger(format!(
            "did not finish within {}s (KRUSTY_DIAGNOSTICS_CASE_TIMEOUT_SECONDS)",
            deadline.as_secs()
        )),
        Reply::Died(status) => Observed::NoLedger(format!("the compiler process died: {status}")),
    }
}

/// One case's ledgers compared.
struct Case {
    key: String,
    expected: usize,
    outcome: Outcome,
}

/// How krusty's report on a case compares with kotlinc's ledger.
enum Outcome {
    /// What kotlinc reports that krusty does not, and what krusty reports that kotlinc does not.
    Compared(Vec<Entry>, Vec<Entry>),
    /// krusty gave no ledger, for this reason.
    NoLedger(String),
    /// The harness could not observe the case, for this reason.
    Unobservable(String),
}

impl Outcome {
    /// Whether krusty's report differs from kotlinc's ledger. A case without a ledger does.
    fn mismatched(&self) -> bool {
        match self {
            Outcome::Compared(absent, added) => !absent.is_empty() || !added.is_empty(),
            Outcome::NoLedger(_) => true,
            Outcome::Unobservable(_) => false,
        }
    }
}

fn run_case(root: &Path, workers: &Workers, file: &Path, version: KotlinVersion) -> Case {
    let key = box_inventory::key(root, file);
    let source = match std::fs::read_to_string(file) {
        Ok(source) => source,
        Err(error) => {
            return Case {
                key,
                expected: 0,
                outcome: Outcome::Unobservable(format!("read {}: {error}", file.display())),
            }
        }
    };
    let expected = expected(&source, version);
    let outcome = match krusty_in_worker(workers, file) {
        Observed::Ledger(reported) => Outcome::Compared(
            difference(&expected, &reported),
            difference(&reported, &expected),
        ),
        Observed::NoLedger(reason) => Outcome::NoLedger(reason),
        Observed::Unobservable(reason) => Outcome::Unobservable(reason),
    };
    Case {
        key,
        expected: expected.len(),
        outcome,
    }
}

/// A krusty worker of [`krusty_reports_kotlinc_s_diagnostics_in_its_diagnostics_test_data`]: it
/// reads case paths from stdin and replies with krusty's ledgers. Without its role variable it does
/// nothing, so an ordinary ignored run passes it.
#[test]
#[ignore = "a krusty worker process of the diagnostics ledger lane"]
fn diagnostics_ledger_worker() {
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
                worker_ledger(&root, &jdks, Path::new(path))
            });
        })
        .expect("start the worker thread");
    worker
        .join()
        .expect("the worker serves until its input closes");
}

fn top(counts: &BTreeMap<String, usize>, limit: usize) -> Vec<(&String, &usize)> {
    let mut sorted: Vec<_> = counts.iter().collect();
    sorted.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
    sorted.truncate(limit);
    sorted
}

#[test]
#[ignore = "compiles ten thousand cases with krusty"]
fn krusty_reports_kotlinc_s_diagnostics_in_its_diagnostics_test_data() {
    let root = box_inventory::kotlin_checkout(CORPORA[0]);
    let version = krusty::kotlin_version::environment_release();
    let workers = Workers {
        test: "kotlin_diagnostics_ledger_jvm_conformance::diagnostics_ledger_worker",
        role: WORKER_ROLE,
        deadline: case_deadline(),
    };
    let cases: Vec<Case> = cases(&root)
        .par_iter()
        .map(|file| run_case(&root, &workers, file, version))
        .collect();

    let mut expected = 0;
    let mut matched = 0;
    let mut matching_cases = 0;
    let mut missing = BTreeMap::<String, usize>::new();
    let mut extra = BTreeMap::<String, usize>::new();
    let mut failed = BTreeMap::<String, usize>::new();
    let mut no_ledger = 0;
    for case in &cases {
        match &case.outcome {
            Outcome::Compared(absent, added) => {
                expected += case.expected;
                matched += case.expected - absent.len();
                for entry in absent {
                    *missing.entry(entry.name.clone()).or_default() += 1;
                }
                for entry in added {
                    *extra.entry(entry.name.clone()).or_default() += 1;
                }
                if absent.is_empty() && added.is_empty() {
                    matching_cases += 1;
                }
            }
            Outcome::NoLedger(reason) => {
                no_ledger += 1;
                *failed.entry(first_line(reason).to_string()).or_default() += 1;
            }
            Outcome::Unobservable(_) => {}
        }
    }
    eprintln!(
        "diagnostics ledger ({version}): {} cases, {matching_cases} report exactly kotlinc's \
         diagnostics; krusty reports {matched} of kotlinc's {expected} diagnostics exactly ({}), \
         misses {} and adds {}; krusty gave no ledger on {no_ledger}",
        cases.len(),
        percent(matched, expected),
        missing.values().sum::<usize>(),
        extra.values().sum::<usize>(),
    );
    for (name, count) in top(&missing, 30) {
        eprintln!("  missing: {count} × {name}");
    }
    for (name, count) in top(&extra, 30) {
        eprintln!("  extra: {count} × {name}");
    }
    for (reason, count) in &failed {
        eprintln!("  no ledger: {count} × {reason}");
    }

    let bless = box_ratchet::bless_requested(
        std::env::var("KRUSTY_BLESS_DIAGNOSTICS_EXPECTATIONS")
            .ok()
            .as_deref(),
    )
    .unwrap_or_else(|error| panic!("{}", error.replace("BOX", "DIAGNOSTICS")));
    assert!(
        !bless || std::env::var_os("CI").is_none(),
        "KRUSTY_BLESS_DIAGNOSTICS_EXPECTATIONS is refused under CI: expectations are blessed locally"
    );
    let path = box_ratchet::read_path(MISMATCHES, "jvm", version);
    let listed = match std::fs::read_to_string(&path) {
        Ok(text) => box_ratchet::parse(&text)
            .unwrap_or_else(|error| panic!("invalid expectation {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => panic!("read {}: {error}", path.display()),
    };
    let problems = judge(&cases, &listed, &path);
    if bless {
        assert!(
            problems.iter().all(|problem| !problem.fatal),
            "the harness could not observe every case, so the list is not rewritten:\n  {}",
            render(&problems)
        );
        let keys: BTreeSet<String> = cases
            .iter()
            .filter(|case| case.outcome.mismatched())
            .map(|case| case.key.clone())
            .collect();
        let mut text = format!(
            "# Kotlin {version} {MISMATCHES_HEADER}\n# Regenerate with KRUSTY_BLESS_DIAGNOSTICS_EXPECTATIONS=1 on a full local run.\n"
        );
        for key in &keys {
            text.push_str(key);
            text.push('\n');
        }
        box_ratchet::record(MISMATCHES, "jvm", version, &keys, &text)
            .unwrap_or_else(|error| panic!("record the {version} {MISMATCHES}: {error}"));
        return;
    }
    assert!(
        problems.is_empty(),
        "{} diagnostics case(s) disagree with {}:\n  {}",
        problems.len(),
        path.display(),
        render(&problems)
    );
    assert!(
        expected > 0,
        "kotlinc's markers name no diagnostic: check the corpus"
    );
}

/// One disagreement between the run and the list. A fatal one is the harness's, not the case's.
#[derive(Debug, PartialEq, Eq)]
struct Problem {
    message: String,
    fatal: bool,
}

fn render(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(|problem| problem.message.as_str())
        .collect::<Vec<_>>()
        .join("\n  ")
}

/// Every disagreement between the run and the listed mismatches. A case the harness could not
/// observe fails the run whatever its listing; a case without a ledger is a mismatch.
fn judge(cases: &[Case], listed: &BTreeSet<String>, path: &Path) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut unobservable = BTreeSet::new();
    for case in cases {
        let key = &case.key;
        let problem = |message: String| Problem {
            message,
            fatal: false,
        };
        match &case.outcome {
            Outcome::Unobservable(reason) => {
                unobservable.insert(key);
                problems.push(Problem {
                    message: format!("{key}: the harness could not observe it ({reason})"),
                    fatal: true,
                });
            }
            outcome if outcome.mismatched() && !listed.contains(key) => {
                problems.push(problem(match outcome {
                    Outcome::NoLedger(reason) => format!(
                        "{key}: krusty newly gives no ledger on it ({})",
                        first_line(reason)
                    ),
                    _ => format!("{key}: krusty's diagnostics newly differ from kotlinc's"),
                }));
            }
            outcome if !outcome.mismatched() && listed.contains(key) => {
                problems.push(problem(format!(
                    "{key}: krusty now reports exactly kotlinc's diagnostics; remove it from {}",
                    path.display()
                )));
            }
            _ => {}
        }
    }
    let corpus: BTreeSet<&String> = cases.iter().map(|case| &case.key).collect();
    for key in listed {
        if !corpus.contains(key) {
            problems.push(Problem {
                message: format!("{key}: listed but absent from the corpus"),
                fatal: false,
            });
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::box_jvm_compile::ReportedDiagnostic;
    use krusty::diag::{names, Span};
    use std::ops::Range;

    fn entry(name: &str, range: Option<Range<usize>>) -> Entry {
        Entry {
            name: name.to_string(),
            range: range.map(|range| (range.start, range.end)),
        }
    }

    #[test]
    fn the_expected_ledger_keeps_only_what_the_command_line_prints() {
        let version = krusty::kotlin_version::environment_release();
        let source = "// DIAGNOSTICS: -UNUSED_VARIABLE\n\
                      fun f() { val <!UNUSED_VARIABLE!>a<!> = <!UNRESOLVED_REFERENCE, DEBUG_INFO_MISSING_UNRESOLVED!>b<!> }\n";
        assert_eq!(
            expected(source, version),
            vec![entry("UNRESOLVED_REFERENCE", Some(51..52))]
        );
        let warnings = "// DIAGNOSTICS: -warnings\n\
                        fun f() { val <!UNUSED_VARIABLE!>a<!> = <!UNRESOLVED_REFERENCE!>b<!> }\n";
        assert_eq!(
            expected(warnings, version),
            vec![entry("UNRESOLVED_REFERENCE", Some(44..45))]
        );
    }

    #[test]
    fn a_check_type_case_leaves_out_the_helper_idiom() {
        let disabled = Disabled::of("// CHECK_TYPE\nfun f() = 1 checkType { _<Int>() }\n");
        assert!(disabled.excludes("UNDERSCORE_USAGE_WITHOUT_BACKTICKS", false));
        assert!(
            !Disabled::of("fun f() = 1\n").excludes("UNDERSCORE_USAGE_WITHOUT_BACKTICKS", false)
        );
    }

    fn diagnostic(name: Option<DiagnosticName>, lo: u32, hi: u32) -> ReportedDiagnostic {
        ReportedDiagnostic {
            name,
            severity: Severity::Error,
            span: Span::new(lo, hi),
        }
    }

    /// The files the harness compiles from `prepared`, each with diagnostics at `spans`.
    fn split(prepared: &str, spans: &[&[(u32, u32)]]) -> Vec<ReportedFile> {
        let (kotlin, _) = krusty::conformance::split_files(prepared).expect("split the case");
        assert_eq!(kotlin.len(), spans.len());
        kotlin
            .into_iter()
            .zip(spans)
            .map(|((_, text), spans)| ReportedFile {
                text,
                diagnostics: spans
                    .iter()
                    .map(|&(lo, hi)| diagnostic(Some(names::UNRESOLVED_REFERENCE), lo, hi))
                    .collect(),
            })
            .collect()
    }

    #[test]
    fn a_reported_file_lands_at_its_position_in_the_case() {
        let version = krusty::kotlin_version::environment_release();
        // Two sections with the same body: each diagnostic lands in its own section.
        let text = "// FILE: a.kt\nval a = b\n// FILE: c.kt\nval a = b\n";
        let prepared = PreparedCase {
            text: format!(
                "// RETURN_VALUE_CHECKER_MODE: FULL\n{text}// FILE: helper.kt\nfun helper() = b\n"
            ),
            origins: vec![
                LineOrigin::Added,
                LineOrigin::Case(0),
                LineOrigin::Case(1),
                LineOrigin::Case(2),
                LineOrigin::Case(3),
                LineOrigin::Added,
                LineOrigin::Added,
            ],
        };
        // a.kt carries the directive line in front of it, and reports on it too; c.kt and the
        // helper are padded to their lines.
        let mut files = split(
            &prepared.text,
            &[&[(0, 2), (57, 58)], &[(25, 26)], &[(39, 40)]],
        );
        assert_eq!(
            files[0].text,
            "// RETURN_VALUE_CHECKER_MODE: FULL\n// FILE: a.kt\nval a = b\n"
        );
        assert_eq!(files[1].text, "\n\n\n// FILE: c.kt\nval a = b\n");
        // A file the harness generates is not laid over the case.
        files.push(ReportedFile {
            text: "fun generated() = b\n".to_string(),
            diagnostics: vec![diagnostic(None, 18, 19)],
        });
        assert_eq!(
            reported(text, &prepared, &files, &Disabled::default(), version),
            vec![
                entry("UNRESOLVED_REFERENCE", Some(22..23)),
                entry("UNRESOLVED_REFERENCE", Some(46..47)),
            ]
        );
    }

    #[test]
    fn a_diagnostic_on_a_rewritten_line_has_no_range() {
        let version = krusty::kotlin_version::environment_release();
        let text = "OPTIONAL_JVM_INLINE_ANNOTATION value class A(val a: B)\n";
        let prepared = PreparedCase {
            text: "@JvmInline value class A(val a: B)\n".to_string(),
            origins: vec![LineOrigin::Rewritten(0)],
        };
        let files = [ReportedFile {
            text: prepared.text.clone(),
            diagnostics: vec![diagnostic(Some(names::UNRESOLVED_REFERENCE), 32, 33)],
        }];
        assert_eq!(
            reported(text, &prepared, &files, &Disabled::default(), version),
            vec![entry("UNRESOLVED_REFERENCE", None)]
        );
    }

    #[test]
    fn ledgers_compare_as_multisets() {
        let a = entry("A", Some(0..1));
        let b = entry("B", Some(2..3));
        assert_eq!(
            difference(&[a.clone(), a.clone(), b.clone()], &[a.clone(), b]),
            vec![a]
        );
    }

    #[test]
    fn a_worker_ledger_round_trips() {
        assert_eq!(decode("L\t"), Observed::Ledger(Vec::new()));
        assert_eq!(
            decode("L\tA\x1e1\x1e2\x1f<unnamed error>\x1e\x1e"),
            Observed::Ledger(vec![entry("A", Some(1..2)), entry("<unnamed error>", None)])
        );
        assert_eq!(decode("F\tno"), Observed::NoLedger("no".to_string()));
        assert_eq!(
            decode("H\tprepare: no"),
            Observed::Unobservable("prepare: no".to_string())
        );
    }

    fn case(key: &str, outcome: Outcome) -> Case {
        Case {
            key: key.to_string(),
            expected: 1,
            outcome,
        }
    }

    fn exact() -> Outcome {
        Outcome::Compared(Vec::new(), Vec::new())
    }

    fn differs() -> Outcome {
        Outcome::Compared(vec![entry("A", Some(0..1))], Vec::new())
    }

    fn problem(message: &str, fatal: bool) -> Problem {
        Problem {
            message: message.to_string(),
            fatal,
        }
    }

    #[test]
    fn a_case_without_a_ledger_is_a_mismatch_that_must_be_listed() {
        let listed = BTreeSet::from([
            "listed-crash.kt".to_string(),
            "listed-exact.kt".to_string(),
            "listed-gone.kt".to_string(),
            "listed-differs.kt".to_string(),
        ]);
        let cases = [
            case("exact.kt", exact()),
            case("differs.kt", differs()),
            case(
                "crash.kt",
                Outcome::NoLedger("panicked: boom\nat".to_string()),
            ),
            case(
                "listed-crash.kt",
                Outcome::NoLedger("timed out".to_string()),
            ),
            case("listed-exact.kt", exact()),
            case("listed-differs.kt", differs()),
        ];
        assert_eq!(
            judge(&cases, &listed, Path::new("list.txt")),
            vec![
                problem(
                    "differs.kt: krusty's diagnostics newly differ from kotlinc's",
                    false
                ),
                problem(
                    "crash.kt: krusty newly gives no ledger on it (panicked: boom)",
                    false
                ),
                problem(
                    "listed-exact.kt: krusty now reports exactly kotlinc's diagnostics; remove it \
                     from list.txt",
                    false
                ),
                problem("listed-gone.kt: listed but absent from the corpus", false),
            ]
        );
    }

    #[test]
    fn a_case_the_harness_cannot_observe_fails_the_run_whatever_its_listing() {
        let listed = BTreeSet::from(["listed.kt".to_string()]);
        let cases = [
            case(
                "listed.kt",
                Outcome::Unobservable("prepare: no helper".to_string()),
            ),
            case(
                "unlisted.kt",
                Outcome::Unobservable("read: gone".to_string()),
            ),
        ];
        assert_eq!(
            judge(&cases, &listed, Path::new("list.txt")),
            vec![
                problem(
                    "listed.kt: the harness could not observe it (prepare: no helper)",
                    true
                ),
                problem(
                    "unlisted.kt: the harness could not observe it (read: gone)",
                    true
                ),
            ]
        );
    }
}
