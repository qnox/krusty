//! Expectations recorded from the reference compiler, one value per supported Kotlin version.
//!
//! kotlinc releases do not agree on everything krusty reproduces (2.4.20 rewords diagnostics and
//! moves some of them), so a test that pins what kotlinc says would otherwise need one hand-written
//! literal per release. Instead [`recorded`] returns the value stored for the running test in
//! `tests/recorded/<test module>.txt` under the reference version under test
//! (`KRUSTY_LANGUAGE_VERSION`, else the newest manifest entry). The test says what it observes
//! through the closure it passes: a whole error ledger, the messages alone, one field of a class
//! file; `tests/common/e2e.rs` wraps the common observations as `assert_*_match_kotlinc`.
//!
//! A version with no stored value is recorded, not guessed: the caller's closure computes it from
//! that version's kotlinc, the value is written into the file, and the diff is committed with the
//! change that needed it. Only kotlinc's output is ever recorded, never krusty's, so a recorded
//! value stays an oracle. Under CI (`CI` set) a missing value fails the test instead, so CI never
//! blesses output nobody reviewed. `KRUSTY_RECORD=1` re-records every case the run reaches.
//!
//! A values file keys each value by a closed version range. Adding a supported Kotlin release
//! therefore leaves every case missing until that release's kotlinc has actually recorded it:
//!
//! ```text
//! [unresolved_member]
//! 2.4.0..2.4.10:
//!   Member.kt:1:27: unresolved reference 'missing'.
//! 2.4.20:
//!   Member.kt:1:27: unresolved reference 'missing' on receiver of type 'String'.
//! ```
//!
//! A range is `lo..hi` (inclusive) or a single version. Each value is a list
//! of lines, each indented by two spaces; a backslash and a line break inside one are written `\\`
//! and `\n`. A range followed by no value lines records an empty list. Recording rewrites a case
//! over the manifest's versions: adjacent versions with equal values merge into one range, and the
//! newest version is always explicit, so a future release cannot silently inherit stale output.

use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use krusty::kotlin_version::{self, KotlinVersion};

/// An inclusive version range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Range {
    lo: KotlinVersion,
    hi: KotlinVersion,
}

impl Range {
    fn contains(self, version: KotlinVersion) -> bool {
        self.lo <= version && version <= self.hi
    }
}

/// One case's ranges, oldest first, each with its value.
type Groups = Vec<(Range, Vec<String>)>;

/// The value the running test has recorded for the reference version under test, computing it
/// with `record` (from that version's kotlinc) and storing it when none is stored.
///
/// The running test names the entry: libtest runs each test on a thread named after its path
/// (`resolver_regression_e2e::a_test`), so the values file is `tests/recorded/<module>.txt` and the
/// case is the rest of the path. Call it from the test's own thread.
pub fn recorded(record: impl FnOnce() -> Vec<String>) -> Vec<String> {
    let (values, case) = running_test();
    recorded_in(&values, &case, record)
}

/// [`recorded`] for a test that records several values: `label` tells them apart.
pub fn recorded_named(label: &str, record: impl FnOnce() -> Vec<String>) -> Vec<String> {
    let (values, case) = running_test();
    recorded_in(&values, &format!("{case}#{label}"), record)
}

/// [`recorded`] for a value that is a single line.
pub fn recorded_line(record: impl FnOnce() -> String) -> String {
    let mut value = recorded(|| vec![record()]);
    assert_eq!(value.len(), 1, "a recorded line must be exactly one line");
    value.remove(0)
}

/// The values file and case of the running test, from its thread's name.
fn running_test() -> (String, String) {
    let thread = std::thread::current();
    let name = thread.name().unwrap_or_default();
    match name.split_once("::") {
        Some((module, case)) => (module.to_string(), case.to_string()),
        None => panic!(
            "recorded values are keyed by the running test's path, but this thread is named \
             {name:?}; call them from the test's own thread"
        ),
    }
}

fn recorded_in(values: &str, case: &str, record: impl FnOnce() -> Vec<String>) -> Vec<String> {
    let target = kotlin_version::target();
    let path = values_path(values);
    let rerecord = std::env::var_os("KRUSTY_RECORD").is_some_and(|flag| flag == "1");
    if !rerecord {
        let stored = std::fs::read_to_string(&path).unwrap_or_default();
        if let Some(value) = lookup(&parse(&stored, &path), case, target) {
            return value;
        }
        assert!(
            std::env::var_os("CI").is_none(),
            "tests/recorded/{values}.txt has no value for `{case}` under Kotlin {target}. Run this \
             test locally with KRUSTY_LANGUAGE_VERSION={target} to record it from kotlinc {target}, \
             then commit the file."
        );
    }
    let value = record();
    store(&path, case, target, &value);
    value
}

fn values_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recorded")
}

fn values_path(values: &str) -> PathBuf {
    values_dir().join(format!("{values}.txt"))
}

fn lookup(
    cases: &BTreeMap<String, Groups>,
    case: &str,
    target: KotlinVersion,
) -> Option<Vec<String>> {
    cases
        .get(case)?
        .iter()
        .find(|(range, _)| range.contains(target))
        .map(|(_, value)| value.clone())
}

/// Write `value` for `case` under `target`, holding an exclusive lock on the values directory so
/// concurrent tests (threads of one binary, or parallel harness shards) never lose each other's
/// rows. The file is replaced by a rename, so an unlocked reader sees the old or the new file.
fn store(path: &Path, case: &str, target: KotlinVersion, value: &[String]) {
    let dir = values_dir();
    std::fs::create_dir_all(&dir).expect("create tests/recorded");
    let lock = std::fs::File::open(&dir).expect("open tests/recorded for locking");
    // SAFETY: `flock` on a descriptor this function owns for the duration of the call.
    let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(locked, 0, "lock tests/recorded");

    let stored = std::fs::read_to_string(path).unwrap_or_default();
    let mut cases = parse(&stored, path);
    let groups = cases.entry(case.to_string()).or_default();
    let supported = KotlinVersion::supported();
    let values: Vec<Option<Vec<String>>> = supported
        .iter()
        .map(|&version| {
            if version == target {
                Some(value.to_vec())
            } else {
                groups
                    .iter()
                    .find(|(range, _)| range.contains(version))
                    .map(|(_, value)| value.clone())
            }
        })
        .collect();
    *groups = ranges(&supported, &values);
    let staging = path.with_extension(format!("txt.{}", std::process::id()));
    std::fs::write(&staging, render(&cases)).expect("write recorded values");
    std::fs::rename(&staging, path).expect("replace recorded values");
    drop(lock);
}

/// Merge adjacent supported versions with equal values into closed ranges.
fn ranges(versions: &[KotlinVersion], values: &[Option<Vec<String>>]) -> Groups {
    let mut groups: Groups = Vec::new();
    let mut previous: Option<&Vec<String>> = None;
    for (&version, value) in versions.iter().zip(values) {
        match value {
            Some(value) if previous == Some(value) => {
                groups.last_mut().expect("an open run").0.hi = version;
            }
            Some(value) => groups.push((
                Range {
                    lo: version,
                    hi: version,
                },
                value.clone(),
            )),
            None => {}
        }
        previous = value.as_ref();
    }
    groups
}

fn parse(text: &str, path: &Path) -> BTreeMap<String, Groups> {
    let mut cases = BTreeMap::<String, Groups>::new();
    let mut case: Option<String> = None;
    for (index, line) in text.lines().enumerate() {
        let malformed =
            || -> ! { panic!("{}:{}: malformed line {line:?}", path.display(), index + 1) };
        if let Some(value) = line.strip_prefix("  ") {
            let groups = case.as_ref().and_then(|case| cases.get_mut(case));
            match groups.and_then(|groups| groups.last_mut()) {
                Some((_, lines)) => lines.push(unescape(value)),
                None => malformed(),
            }
        } else if line.is_empty() || line.starts_with('#') {
            continue;
        } else if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            cases.entry(name.to_string()).or_default();
            case = Some(name.to_string());
        } else if let Some(range) = line.strip_suffix(':') {
            let version = |text: &str| KotlinVersion::parse(text).unwrap_or_else(|| malformed());
            let range = match range.split_once("..") {
                Some((lo, hi)) => Range {
                    lo: version(lo),
                    hi: version(hi),
                },
                None => Range {
                    lo: version(range),
                    hi: version(range),
                },
            };
            match case.as_ref().and_then(|case| cases.get_mut(case)) {
                Some(groups) => groups.push((range, Vec::new())),
                None => malformed(),
            }
        } else {
            malformed();
        }
    }
    cases
}

fn render(cases: &BTreeMap<String, Groups>) -> String {
    let mut text = String::from(
        "# kotlinc output recorded per Kotlin version by tests/common/recorded.rs. A version with\n\
         # no value is recorded from its kotlinc on the next local run; KRUSTY_RECORD=1 re-records.\n",
    );
    for (case, groups) in cases {
        text.push_str(&format!("\n[{case}]\n"));
        for (range, value) in groups {
            let range = match range.hi {
                hi if hi == range.lo => hi.to_string(),
                hi => format!("{}..{hi}", range.lo),
            };
            text.push_str(&format!("{range}:\n"));
            for line in value {
                text.push_str(&format!("  {}\n", escape(line)));
            }
        }
    }
    text
}

fn escape(line: &str) -> String {
    line.replace('\\', "\\\\").replace('\n', "\\n")
}

fn unescape(line: &str) -> String {
    let mut text = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (c, c == '\\') {
            (_, true) => match chars.next() {
                Some('n') => text.push('\n'),
                Some(other) => text.push(other),
                None => text.push('\\'),
            },
            (c, false) => text.push(c),
        }
    }
    text
}

#[test]
fn a_values_file_round_trips_and_keeps_the_newest_version_explicit() {
    let v = |text| KotlinVersion::parse(text).unwrap();
    let old = vec!["a\\b\nc".to_string()];
    let groups = ranges(
        &[v("2.4.0"), v("2.4.10"), v("2.4.20"), v("2.4.30")],
        &[
            Some(old.clone()),
            Some(old.clone()),
            Some(Vec::new()),
            Some(Vec::new()),
        ],
    );
    let mut cases = BTreeMap::new();
    cases.insert("case".to_string(), groups);
    let text = render(&cases);
    assert!(
        text.contains("[case]\n2.4.0..2.4.10:\n  a\\\\b\\nc\n2.4.20..2.4.30:\n"),
        "{text}"
    );
    let parsed = parse(&text, Path::new("test"));
    assert_eq!(parsed, cases);
    assert_eq!(lookup(&parsed, "case", v("2.4.10")), Some(old));
    assert_eq!(lookup(&parsed, "case", v("2.5.0")), None);
    assert_eq!(lookup(&parsed, "case", v("2.3.0")), None);
    assert!(
        std::panic::catch_unwind(|| { parse("[case]\n2.4.20..:\n", Path::new("open-ended")) })
            .is_err()
    );
    let gap = ranges(&[v("2.4.0"), v("2.4.10")], &[Some(Vec::new()), None]);
    assert_eq!(
        gap,
        [(
            Range {
                lo: v("2.4.0"),
                hi: v("2.4.0")
            },
            Vec::new()
        )]
    );
}
