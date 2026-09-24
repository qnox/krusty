//! Exact per-version baselines for Kotlin codegen/box outcomes.
//!
//! Every corpus file has one expected outcome: pass (implicit), fail (listed in
//! `box_expected_failures`), or not applicable to the JVM backend (listed in
//! `box_expected_not_applicable`). A transition between any two outcomes fails the run. This keeps
//! an applicability regression from looking like a successful test run.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use krusty::kotlin_version::KotlinVersion;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    NotApplicable,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotApplicable => "not-applicable",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Baseline {
    pub failures: BTreeSet<String>,
    pub not_applicable: BTreeSet<String>,
}

pub fn list_path(version: KotlinVersion) -> PathBuf {
    manifest_path("box_expected_failures", version)
}

pub fn not_applicable_list_path(version: KotlinVersion) -> PathBuf {
    manifest_path("box_expected_not_applicable", version)
}

fn manifest_path(directory: &str, version: KotlinVersion) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(directory)
        .join(format!("{version}.txt"))
}

pub fn load(version: KotlinVersion) -> Baseline {
    Baseline {
        failures: load_path(&list_path(version)),
        not_applicable: load_path(&not_applicable_list_path(version)),
    }
}

fn load_path(path: &Path) -> BTreeSet<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text)
            .unwrap_or_else(|error| panic!("invalid outcome manifest {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => panic!("failed to read {}: {error}", path.display()),
    }
}

pub fn parse(text: &str) -> Result<BTreeSet<String>, String> {
    let mut paths = BTreeSet::new();
    for (line_number, line) in text.lines().enumerate() {
        let path = line.trim();
        if path.is_empty() || path.starts_with('#') {
            continue;
        }
        if !paths.insert(path.to_owned()) {
            return Err(format!(
                "duplicate path on line {}: {path}",
                line_number + 1
            ));
        }
    }
    Ok(paths)
}

pub fn render_failures(version: KotlinVersion, paths: &BTreeSet<String>) -> String {
    render(
        version,
        "fail",
        "box_expected_failures",
        "expected failures",
        paths,
    )
}

pub fn render_not_applicable(version: KotlinVersion, paths: &BTreeSet<String>) -> String {
    render(
        version,
        "be not-applicable to the JVM backend",
        "box_expected_not_applicable",
        "expected not-applicable cases",
        paths,
    )
}

fn render(
    version: KotlinVersion,
    expected: &str,
    directory: &str,
    description: &str,
    paths: &BTreeSet<String>,
) -> String {
    let mut text = format!(
        "# Kotlin {version} codegen/box files krusty expects to {expected}, relative to\n\
         # compiler/testData/codegen/box. Every outcome transition fails conformance. Regenerate\n\
         # tests/{directory}/{version}.txt with KRUSTY_BLESS_BOX_FAILURES=1 on a full local run.\n\
         # The goal is to empty this list of {description}.\n"
    );
    for path in paths {
        text.push_str(path);
        text.push('\n');
    }
    text
}

pub fn bless_requested(value: Option<&str>) -> Result<bool, String> {
    match value {
        None => Ok(false),
        Some("1") => Ok(true),
        Some(value) => Err(format!(
            "KRUSTY_BLESS_BOX_FAILURES must be exactly 1 when set, got {value:?}"
        )),
    }
}

/// Replace one manifest without exposing a truncated or partially written tracked file.
pub fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "manifest has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    let leaf = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "manifest has no file name",
        )
    })?;
    let mut attempt = 0_u8;
    loop {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{}.{}.{}.tmp",
            leaf.to_string_lossy(),
            std::process::id(),
            sequence
        ));
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary);
        let mut file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt < 8 => {
                attempt += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        let result = (|| {
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        return result;
    }
}

pub fn corpus_key(box_dir: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(box_dir).unwrap_or(file);
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Mismatches {
    pub outcome_changes: Vec<String>,
    pub overlapping: Vec<String>,
    pub unknown: Vec<String>,
}

impl Mismatches {
    pub fn is_empty(&self) -> bool {
        self.outcome_changes.is_empty() && self.overlapping.is_empty() && self.unknown.is_empty()
    }

    pub fn report(&self, version: KotlinVersion) -> String {
        let mut text = String::new();
        for (title, entries) in [
            ("outcome changed", &self.outcome_changes),
            ("listed in both outcome manifests", &self.overlapping),
            ("listed but absent from the corpus", &self.unknown),
        ] {
            if entries.is_empty() {
                continue;
            }
            let _ = writeln!(text, "{} file(s) {title}:", entries.len());
            for entry in entries {
                let _ = writeln!(text, "  {entry}");
            }
        }
        let _ = write!(
            text,
            "box conformance disagrees with {} and {}; update both in the same change \
             (KRUSTY_BLESS_BOX_FAILURES=1 on a full local run rewrites them)",
            list_path(version).display(),
            not_applicable_list_path(version).display(),
        );
        text
    }
}

/// Compare only scheduled files, so a filtered or sharded run checks its own slice. `corpus` is the
/// complete discovered set and catches stale manifest entries even when their shard is not running.
pub fn compare(
    expected: &Baseline,
    outcomes: &BTreeMap<String, Outcome>,
    corpus: &BTreeSet<String>,
) -> Mismatches {
    let mut mismatches = Mismatches::default();
    for (path, actual) in outcomes {
        let expected_outcome = if expected.failures.contains(path) {
            Outcome::Fail
        } else if expected.not_applicable.contains(path) {
            Outcome::NotApplicable
        } else {
            Outcome::Pass
        };
        if *actual != expected_outcome {
            mismatches.outcome_changes.push(format!(
                "{path}: expected {}, got {}",
                expected_outcome.label(),
                actual.label()
            ));
        }
    }
    mismatches.overlapping = expected
        .failures
        .intersection(&expected.not_applicable)
        .cloned()
        .collect();
    mismatches.unknown = expected
        .failures
        .union(&expected.not_applicable)
        .filter(|path| !corpus.contains(*path))
        .cloned()
        .collect();
    mismatches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    fn outcomes(entries: &[(&str, Outcome)]) -> BTreeMap<String, Outcome> {
        entries
            .iter()
            .map(|(path, outcome)| (path.to_string(), *outcome))
            .collect()
    }

    #[test]
    fn every_outcome_transition_is_reported_exactly() {
        let expected = Baseline {
            failures: set(&["fail-to-pass.kt", "fail-to-na.kt"]),
            not_applicable: set(&["na-to-pass.kt", "na-to-fail.kt"]),
        };
        let run = outcomes(&[
            ("pass-to-fail.kt", Outcome::Fail),
            ("pass-to-na.kt", Outcome::NotApplicable),
            ("fail-to-pass.kt", Outcome::Pass),
            ("fail-to-na.kt", Outcome::NotApplicable),
            ("na-to-pass.kt", Outcome::Pass),
            ("na-to-fail.kt", Outcome::Fail),
        ]);
        let corpus = run.keys().cloned().collect();
        assert_eq!(
            compare(&expected, &run, &corpus).outcome_changes,
            [
                "fail-to-na.kt: expected fail, got not-applicable",
                "fail-to-pass.kt: expected fail, got pass",
                "na-to-fail.kt: expected not-applicable, got fail",
                "na-to-pass.kt: expected not-applicable, got pass",
                "pass-to-fail.kt: expected pass, got fail",
                "pass-to-na.kt: expected pass, got not-applicable",
            ]
        );
    }

    #[test]
    fn a_matching_shard_does_not_judge_other_scheduled_slices() {
        let expected = Baseline {
            failures: set(&["other-failure.kt"]),
            not_applicable: set(&["other-na.kt"]),
        };
        let run = outcomes(&[("mine.kt", Outcome::Pass)]);
        let corpus = set(&["mine.kt", "other-failure.kt", "other-na.kt"]);
        assert_eq!(compare(&expected, &run, &corpus), Mismatches::default());
    }

    #[test]
    fn malformed_and_stale_baselines_have_an_exact_report() {
        let version = KotlinVersion::V2_4_10;
        let expected = Baseline {
            failures: set(&["both.kt", "deleted.kt"]),
            not_applicable: set(&["both.kt"]),
        };
        let mismatches = compare(&expected, &BTreeMap::new(), &set(&["both.kt"]));
        assert_eq!(
            mismatches.report(version),
            format!(
                "1 file(s) listed in both outcome manifests:\n  both.kt\n\
                 1 file(s) listed but absent from the corpus:\n  deleted.kt\n\
                 box conformance disagrees with {} and {}; update both in the same change \
                 (KRUSTY_BLESS_BOX_FAILURES=1 on a full local run rewrites them)",
                list_path(version).display(),
                not_applicable_list_path(version).display(),
            )
        );
    }

    #[test]
    fn rendered_manifests_are_exact_and_round_trip() {
        let paths = set(&["b/two.kt", "a/one.kt"]);
        let text = render_failures(KotlinVersion::V2_4_10, &paths);
        assert_eq!(
            text,
            "# Kotlin 2.4.10 codegen/box files krusty expects to fail, relative to\n\
             # compiler/testData/codegen/box. Every outcome transition fails conformance. Regenerate\n\
             # tests/box_expected_failures/2.4.10.txt with KRUSTY_BLESS_BOX_FAILURES=1 on a full local run.\n\
             # The goal is to empty this list of expected failures.\n\
             a/one.kt\n\
             b/two.kt\n"
        );
        assert_eq!(parse(&text), Ok(paths));
    }

    #[test]
    fn duplicate_manifest_paths_are_rejected_exactly() {
        assert_eq!(
            parse("# baseline\na/one.kt\n\na/one.kt\n"),
            Err("duplicate path on line 4: a/one.kt".into())
        );
    }

    #[test]
    fn blessing_requires_the_exact_opt_in_value() {
        assert_eq!(bless_requested(None), Ok(false));
        assert_eq!(bless_requested(Some("1")), Ok(true));
        assert_eq!(
            bless_requested(Some("false")),
            Err("KRUSTY_BLESS_BOX_FAILURES must be exactly 1 when set, got \"false\"".into())
        );
        assert_eq!(
            bless_requested(Some("0")),
            Err("KRUSTY_BLESS_BOX_FAILURES must be exactly 1 when set, got \"0\"".into())
        );
    }

    #[test]
    fn atomic_write_replaces_the_manifest_without_leaving_a_staging_file() {
        let root = std::env::temp_dir().join(format!(
            "krusty-box-ratchet-{}-{}",
            std::process::id(),
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let path = root.join("baseline.txt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&path, "old\n").unwrap();
        write_atomic(&path, "new\ncomplete\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\ncomplete\n");
        assert_eq!(
            std::fs::read_dir(&root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            [std::ffi::OsString::from("baseline.txt")]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_corpus_key_is_relative_and_slash_separated() {
        let root = Path::new("/corpus/box");
        assert_eq!(
            corpus_key(root, Path::new("/corpus/box/inline/a.kt")),
            "inline/a.kt"
        );
    }
}
