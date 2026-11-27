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
//! [`recording_diagnostics`]: each compiled file is located in the case's marker-free text, so a
//! diagnostic of a `// FILE:` section or a `// MODULE:` unit lands at its offset in the case. Like
//! JetBrains' runner, the lane checks only the case's own files, not the helpers a directive adds,
//! and leaves out `UNDERSCORE_USAGE_WITHOUT_BACKTICKS` where `// CHECK_TYPE` adds its helper, whose
//! `checkType { _<T>() }` idiom kotlinc's command line reports.
//!
//! The cases whose ledgers differ ratchet in `tests/diagnostics_ledger_mismatches/jvm/<version>.txt`:
//! a case joining or leaving the list fails until the list is updated, and
//! `KRUSTY_BLESS_DIAGNOSTICS_EXPECTATIONS=1` rewrites it from a full local run. A case krusty gives
//! no ledger on (a crash, a timeout, a topology the harness cannot compile) keeps its listing
//! either way and is counted. The run prints the diagnostic totals and the names krusty misses or
//! adds most, the measure the list's size alone does not give.

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
    box_jdks, case_deadline, case_stem, cases, first_line, panic_message, percent, prepare, CORPORA,
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

/// krusty's ledger of a case whose marker-free text is `text`, from the files its compilations
/// reported in. A named diagnostic reported with a severity other than its factory's keeps the
/// difference in its entry's name, as does a diagnostic krusty does not name.
fn reported(
    text: &str,
    files: &[ReportedFile],
    disabled: &Disabled,
    version: KotlinVersion,
) -> Vec<Entry> {
    let mut entries = Vec::new();
    for file in files {
        // Where the file starts in the case: a `// FILE:` section or `// MODULE:` unit is a slice
        // of it, and a case's directory defaults or helper files put text in front of it.
        let origin = match text.find(file.text.as_str()) {
            Some(offset) => offset as isize,
            None => match file.text.find(text) {
                Some(offset) => -(offset as isize),
                // A helper file the runner adds: JetBrains' runner checks only the case's own files.
                None => continue,
            },
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
            // A diagnostic in text the case does not contain (the directory defaults in front of
            // it) has no range, and never matches a marker.
            let lo = usize::try_from(origin + diagnostic.span.lo as isize).ok();
            let hi = usize::try_from(origin + diagnostic.span.hi as isize).ok();
            let range = lo.zip(hi);
            entries.push(Entry { name, range });
        }
    }
    entries.sort();
    entries
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
/// `name\x1elo\x1ehi` record per entry, separated by `\x1f`, or `F` and why it has none.
fn worker_ledger(root: &Path, jdks: &BoxJdkRoots, file: &Path) -> String {
    let source = match std::fs::read_to_string(file) {
        Ok(source) => source,
        Err(error) => return format!("F\tread {}: {error}", file.display()),
    };
    let stem = case_stem(file);
    let prepared = match prepare(root, file, &source, stem) {
        Ok(prepared) => prepared,
        Err(reason) => return format!("F\t{reason}"),
    };
    let classpath = common::classpath_jars_for(&prepared);
    let jdk = jdks.select(&prepared);
    let (compiled, files) = recording_diagnostics(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compile_box_case(&prepared, stem, &classpath, jdk.classpath_root(), &|_| {})
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
        &files,
        &Disabled::of(&source),
        krusty::kotlin_version::target(),
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

fn decode(line: &str) -> Result<Vec<Entry>, String> {
    match line.split_once('\t') {
        Some(("L", "")) => Ok(Vec::new()),
        Some(("L", records)) => Ok(records
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
            .collect()),
        Some(("F", reason)) => Err(reason.to_string()),
        _ => panic!("a ledger worker replied {line:?}"),
    }
}

fn krusty_in_worker(workers: &Workers, file: &Path) -> Result<Vec<Entry>, String> {
    match workers.request(&file.display().to_string()) {
        Reply::Line(line) => decode(&line),
        Reply::TimedOut(deadline) => Err(format!(
            "did not finish within {}s (KRUSTY_DIAGNOSTICS_CASE_TIMEOUT_SECONDS)",
            deadline.as_secs()
        )),
        Reply::Died(status) => Err(format!("the compiler process died: {status}")),
    }
}

/// One case's ledgers compared.
struct Case {
    key: String,
    expected: usize,
    /// What kotlinc reports that krusty does not, and what krusty reports that kotlinc does not;
    /// `Err` when krusty gave no ledger.
    difference: Result<(Vec<Entry>, Vec<Entry>), String>,
}

fn run_case(root: &Path, workers: &Workers, file: &Path, version: KotlinVersion) -> Case {
    let key = box_inventory::key(root, file);
    let source = std::fs::read_to_string(file)
        .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
    let expected = expected(&source, version);
    let difference = krusty_in_worker(workers, file).map(|reported| {
        (
            difference(&expected, &reported),
            difference(&reported, &expected),
        )
    });
    Case {
        key,
        expected: expected.len(),
        difference,
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
    let version = krusty::kotlin_version::target();
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
    let mut mismatched = BTreeSet::new();
    let mut unobserved = BTreeSet::new();
    for case in &cases {
        match &case.difference {
            Ok((absent, added)) => {
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
                } else {
                    mismatched.insert(case.key.clone());
                }
            }
            Err(reason) => {
                *failed.entry(first_line(reason).to_string()).or_default() += 1;
                unobserved.insert(case.key.clone());
            }
        }
    }
    eprintln!(
        "diagnostics ledger ({version}): {} cases, {matching_cases} report exactly kotlinc's \
         diagnostics; krusty reports {matched} of kotlinc's {expected} diagnostics exactly ({}), \
         misses {} and adds {}; krusty gave no ledger on {}",
        cases.len(),
        percent(matched, expected),
        missing.values().sum::<usize>(),
        extra.values().sum::<usize>(),
        unobserved.len(),
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
    let corpus: BTreeSet<&String> = cases.iter().map(|case| &case.key).collect();
    let problems = judge(&mismatched, &unobserved, &corpus, &listed, &path);
    if bless {
        // A case without a ledger keeps whatever its listing was.
        let keys: BTreeSet<String> = mismatched
            .iter()
            .chain(listed.intersection(&unobserved))
            .cloned()
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
        problems.join("\n  ")
    );
    assert!(
        expected > 0,
        "kotlinc's markers name no diagnostic: check the corpus"
    );
}

/// Every disagreement between the observed mismatches and the listed ones.
fn judge(
    mismatched: &BTreeSet<String>,
    unobserved: &BTreeSet<String>,
    corpus: &BTreeSet<&String>,
    listed: &BTreeSet<String>,
    path: &Path,
) -> Vec<String> {
    let mut problems = Vec::new();
    for key in mismatched.difference(listed) {
        problems.push(format!(
            "{key}: krusty's diagnostics newly differ from kotlinc's"
        ));
    }
    for key in listed.difference(mismatched) {
        if unobserved.contains(key) {
            continue;
        }
        if corpus.contains(key) {
            problems.push(format!(
                "{key}: krusty now reports exactly kotlinc's diagnostics; remove it from {}",
                path.display()
            ));
        } else {
            problems.push(format!("{key}: listed but absent from the corpus"));
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
        let version = krusty::kotlin_version::target();
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

    #[test]
    fn a_reported_file_lands_at_its_offset_in_the_case() {
        let version = krusty::kotlin_version::target();
        let text = "// FILE: a.kt\nval a = b\n// FILE: c.kt\nval c = d\n";
        let diagnostic = |name, lo, hi| ReportedDiagnostic {
            name,
            severity: Severity::Error,
            span: Span::new(lo, hi),
        };
        let files = [
            ReportedFile {
                text: "val c = d\n".to_string(),
                diagnostics: vec![diagnostic(Some(names::UNRESOLVED_REFERENCE), 8, 9)],
            },
            ReportedFile {
                text: format!("// RETURN_VALUE_CHECKER_MODE: FULL\n{text}"),
                diagnostics: vec![diagnostic(None, 43, 44)],
            },
            ReportedFile {
                text: "fun helper() = 1\n".to_string(),
                diagnostics: vec![diagnostic(None, 0, 3)],
            },
        ];
        assert_eq!(
            reported(text, &files, &Disabled::default(), version),
            vec![
                entry("<unnamed error>", Some(8..9)),
                entry("UNRESOLVED_REFERENCE", Some(46..47)),
            ]
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
        assert_eq!(decode("L\t"), Ok(Vec::new()));
        assert_eq!(
            decode("L\tA\x1e1\x1e2\x1f<unnamed error>\x1e\x1e"),
            Ok(vec![entry("A", Some(1..2)), entry("<unnamed error>", None)])
        );
        assert_eq!(decode("F\tno"), Err("no".to_string()));
    }

    #[test]
    fn a_case_without_a_ledger_keeps_its_listing() {
        let listed = BTreeSet::from(["x.kt".to_string(), "y.kt".to_string(), "z.kt".to_string()]);
        let x = "x.kt".to_string();
        let y = "y.kt".to_string();
        let z = "z.kt".to_string();
        let w = "w.kt".to_string();
        let corpus = BTreeSet::from([&x, &y, &z, &w]);
        let problems = judge(
            &BTreeSet::from([x.clone(), w.clone()]),
            &BTreeSet::from([y.clone()]),
            &corpus,
            &listed,
            Path::new("list.txt"),
        );
        assert_eq!(
            problems,
            vec![
                "w.kt: krusty's diagnostics newly differ from kotlinc's".to_string(),
                "z.kt: krusty now reports exactly kotlinc's diagnostics; remove it from list.txt"
                    .to_string(),
            ]
        );
    }
}
