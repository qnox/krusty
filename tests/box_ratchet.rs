//! Generic per-platform, exact-version box-conformance ratchet.
//!
//! A corpus path has one of three outcomes: pass (implicit), fail, or not applicable. Expected
//! failures and applicability exclusions are committed as deterministic text, so a local run and
//! CI enforce the same baseline. The platform is part of the path; one backend can neither hide nor
//! inherit another backend's backlog.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use krusty::kotlin_version::KotlinVersion;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Jvm,
    Native,
    WasmJs,
    WasmWasi,
}

impl Platform {
    pub fn name(self) -> &'static str {
        match self {
            Self::Jvm => "jvm",
            Self::Native => "native",
            Self::WasmJs => "wasm-js",
            Self::WasmWasi => "wasm-wasi",
        }
    }
}

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

pub fn failure_path(platform: Platform, version: KotlinVersion) -> PathBuf {
    manifest_path("box_expected_failures", platform, version)
}

pub fn not_applicable_path(platform: Platform, version: KotlinVersion) -> PathBuf {
    manifest_path("box_expected_not_applicable", platform, version)
}

fn manifest_path(directory: &str, platform: Platform, version: KotlinVersion) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(directory)
        .join(platform.name())
        .join(format!("{version}.txt"))
}

pub fn load(platform: Platform, version: KotlinVersion) -> Baseline {
    Baseline {
        // JVM has no failure ratchet: absence means exactly the required empty set. Every other
        // lane's failure inventory is part of its committed baseline and may never disappear
        // silently.
        failures: load_path(&failure_path(platform, version), platform != Platform::Jvm),
        not_applicable: load_path(&not_applicable_path(platform, version), true),
    }
}

fn load_path(path: &Path, required: bool) -> BTreeSet<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text)
            .unwrap_or_else(|error| panic!("invalid box expectation {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => BTreeSet::new(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            panic!("required box expectation is missing: {}", path.display())
        }
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

pub fn render_failures(
    platform: Platform,
    version: KotlinVersion,
    paths: &BTreeSet<String>,
) -> String {
    render(platform, version, "fail", paths)
}

pub fn render_not_applicable(
    platform: Platform,
    version: KotlinVersion,
    paths: &BTreeSet<String>,
) -> String {
    render(platform, version, "be not-applicable", paths)
}

fn render(
    platform: Platform,
    version: KotlinVersion,
    expected: &str,
    paths: &BTreeSet<String>,
) -> String {
    let mut text = format!(
        "# Kotlin {version} codegen/box files expected to {expected} on {}, relative to\n\
         # compiler/testData/codegen/box. Regenerate both exact inventories with\n\
         # KRUSTY_BLESS_BOX_EXPECTATIONS=1 on a full local {} run. The lists only shrink.\n",
        platform.name(),
        platform.name(),
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
            "KRUSTY_BLESS_BOX_EXPECTATIONS must be exactly 1 when set, got {value:?}"
        )),
    }
}

pub fn write(
    platform: Platform,
    version: KotlinVersion,
    outcomes: &BTreeMap<String, Outcome>,
) -> std::io::Result<()> {
    let failures = outcomes
        .iter()
        .filter(|(_, outcome)| **outcome == Outcome::Fail)
        .map(|(path, _)| path.clone())
        .collect();
    let not_applicable = outcomes
        .iter()
        .filter(|(_, outcome)| **outcome == Outcome::NotApplicable)
        .map(|(path, _)| path.clone())
        .collect();
    write_atomic(
        &failure_path(platform, version),
        &render_failures(platform, version, &failures),
    )?;
    write_atomic(
        &not_applicable_path(platform, version),
        &render_not_applicable(platform, version, &not_applicable),
    )
}

pub fn write_not_applicable(
    platform: Platform,
    version: KotlinVersion,
    paths: &BTreeSet<String>,
) -> std::io::Result<()> {
    write_atomic(
        &not_applicable_path(platform, version),
        &render_not_applicable(platform, version, paths),
    )
}

/// Replace one inventory without exposing a truncated or partially written tracked file.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
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

    pub fn report(&self, platform: Platform, version: KotlinVersion) -> String {
        let mut text = String::new();
        for (title, entries) in [
            ("outcome changed", &self.outcome_changes),
            (
                "is listed as both failure and not-applicable",
                &self.overlapping,
            ),
            ("is listed but absent from the corpus", &self.unknown),
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
            "{} box conformance disagrees with {} and {}; update them from one full local run with \
             KRUSTY_BLESS_BOX_EXPECTATIONS=1",
            platform.name(),
            failure_path(platform, version).display(),
            not_applicable_path(platform, version).display(),
        );
        text
    }
}

/// Compare scheduled paths only, while validating committed entries against the complete corpus.
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

    #[test]
    fn platform_and_version_are_both_part_of_the_paths() {
        assert!(failure_path(Platform::Jvm, KotlinVersion::V2_4_20)
            .ends_with("tests/box_expected_failures/jvm/2.4.20.txt"));
        assert!(
            not_applicable_path(Platform::Native, KotlinVersion::V2_4_10)
                .ends_with("tests/box_expected_not_applicable/native/2.4.10.txt")
        );
    }

    #[test]
    fn every_outcome_transition_is_reported() {
        let expected = Baseline {
            failures: set(&["fail-to-pass.kt", "fail-to-na.kt"]),
            not_applicable: set(&["na-to-pass.kt", "na-to-fail.kt"]),
        };
        let outcomes = BTreeMap::from([
            ("pass-to-fail.kt".into(), Outcome::Fail),
            ("pass-to-na.kt".into(), Outcome::NotApplicable),
            ("fail-to-pass.kt".into(), Outcome::Pass),
            ("fail-to-na.kt".into(), Outcome::NotApplicable),
            ("na-to-pass.kt".into(), Outcome::Pass),
            ("na-to-fail.kt".into(), Outcome::Fail),
        ]);
        let corpus = outcomes.keys().cloned().collect();
        assert_eq!(
            compare(&expected, &outcomes, &corpus).outcome_changes,
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
    fn a_matching_focused_selection_does_not_judge_unselected_cases() {
        let expected = Baseline {
            failures: set(&["other-failure.kt"]),
            not_applicable: set(&["other-na.kt"]),
        };
        let outcomes = BTreeMap::from([("mine.kt".into(), Outcome::Pass)]);
        let corpus = set(&["mine.kt", "other-failure.kt", "other-na.kt"]);
        assert_eq!(
            compare(&expected, &outcomes, &corpus),
            Mismatches::default()
        );
    }

    #[test]
    fn rendered_inventory_is_sorted_and_round_trips() {
        let paths = set(&["b/two.kt", "a/one.kt"]);
        let text = render_failures(Platform::Native, KotlinVersion::V2_4_10, &paths);
        assert_eq!(parse(&text), Ok(paths));
        assert!(text.find("a/one.kt").unwrap() < text.find("b/two.kt").unwrap());
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
            mismatches.report(Platform::Native, version),
            format!(
                "1 file(s) is listed as both failure and not-applicable:\n  both.kt\n\
                 1 file(s) is listed but absent from the corpus:\n  deleted.kt\n\
                 native box conformance disagrees with {} and {}; update them from one full local run with \
                 KRUSTY_BLESS_BOX_EXPECTATIONS=1",
                failure_path(Platform::Native, version).display(),
                not_applicable_path(Platform::Native, version).display(),
            )
        );
    }

    #[test]
    fn duplicate_paths_are_rejected() {
        assert_eq!(
            parse("a/one.kt\na/one.kt\n"),
            Err("duplicate path on line 2: a/one.kt".into())
        );
    }

    #[test]
    fn blessing_requires_the_exact_opt_in_value() {
        assert_eq!(bless_requested(None), Ok(false));
        assert_eq!(bless_requested(Some("1")), Ok(true));
        assert_eq!(
            bless_requested(Some("false")),
            Err("KRUSTY_BLESS_BOX_EXPECTATIONS must be exactly 1 when set, got \"false\"".into())
        );
    }
}
