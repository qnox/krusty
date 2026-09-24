//! The box-corpus ratchet: which codegen/box files krusty is still allowed to fail.
//!
//! Each supported Kotlin reference version has a list, `tests/box_expected_failures/<version>.txt`,
//! of corpus paths relative to `compiler/testData/codegen/box`. A conformance run fails when a file
//! outside the list fails (a regression) or a file in the list passes (a fix that must also remove
//! the entry). A listed path the corpus does not have, or one the backend does not run, is a stale
//! entry and fails too. A version without a list expects every applicable file to pass: the goal is
//! to empty every list and then delete this module.
//!
//! `KRUSTY_BLESS_BOX_FAILURES=1` on a full, unfiltered run rewrites the list to the files that failed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use krusty::kotlin_version::KotlinVersion;

/// What one scheduled corpus file did in this run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    NotApplicable,
}

/// The expected-failure list of `version`.
pub fn list_path(version: KotlinVersion) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("box_expected_failures")
        .join(format!("{version}.txt"))
}

/// Read `version`'s list; a missing file is an empty list.
pub fn load(version: KotlinVersion) -> BTreeSet<String> {
    let path = list_path(version);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => panic!("failed to read {}: {error}", path.display()),
    }
}

/// One path per line; blank lines and `#` comments are ignored.
pub fn parse(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The list file for `version` naming exactly `failing`.
pub fn render(version: KotlinVersion, failing: &BTreeSet<String>) -> String {
    let mut text = format!(
        "# Kotlin {version} codegen/box files krusty is expected to fail, relative to\n\
         # compiler/testData/codegen/box. The conformance test fails when a file outside this list\n\
         # fails or a file in it passes. Regenerate with KRUSTY_BLESS_BOX_FAILURES=1 on a full run.\n\
         # The goal is an empty list.\n"
    );
    for path in failing {
        text.push_str(path);
        text.push('\n');
    }
    text
}

/// A corpus file's key: its path under the corpus root, `/`-separated.
pub fn corpus_key(box_dir: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(box_dir).unwrap_or(file);
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Every way this run disagrees with the list.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Mismatches {
    /// Failed, not listed: a regression.
    pub unexpected_failures: Vec<String>,
    /// Listed, passed: remove the entry.
    pub unexpected_passes: Vec<String>,
    /// Listed, but the backend does not run the file.
    pub not_applicable: Vec<String>,
    /// Listed, but not in the corpus at all.
    pub unknown: Vec<String>,
}

impl Mismatches {
    pub fn is_empty(&self) -> bool {
        self.unexpected_failures.is_empty()
            && self.unexpected_passes.is_empty()
            && self.not_applicable.is_empty()
            && self.unknown.is_empty()
    }

    pub fn report(&self, version: KotlinVersion) -> String {
        let mut text = String::new();
        let sections = [
            (
                "failed but not listed as an expected failure (regression)",
                &self.unexpected_failures,
            ),
            (
                "listed as an expected failure but passed (remove the entry)",
                &self.unexpected_passes,
            ),
            (
                "listed but not applicable to the JVM backend (remove the entry)",
                &self.not_applicable,
            ),
            (
                "listed but absent from the corpus (remove the entry)",
                &self.unknown,
            ),
        ];
        for (title, paths) in sections {
            if paths.is_empty() {
                continue;
            }
            let _ = writeln!(text, "{} file(s) {title}:", paths.len());
            for path in paths {
                let _ = writeln!(text, "  {path}");
            }
        }
        let _ = write!(
            text,
            "box conformance disagrees with {}; update it in the same change \
             (KRUSTY_BLESS_BOX_FAILURES=1 on a full run rewrites it)",
            list_path(version).display()
        );
        text
    }
}

/// Compare the scheduled files' outcomes against `expected`. Only scheduled files are judged, so a
/// sharded or filtered run checks its own slice; `corpus` (every discovered file) catches entries
/// that name no file at all.
pub fn compare(
    expected: &BTreeSet<String>,
    outcomes: &BTreeMap<String, Outcome>,
    corpus: &BTreeSet<String>,
) -> Mismatches {
    let mut mismatches = Mismatches::default();
    for (path, outcome) in outcomes {
        let listed = expected.contains(path);
        match outcome {
            Outcome::Fail if !listed => mismatches.unexpected_failures.push(path.clone()),
            Outcome::Pass if listed => mismatches.unexpected_passes.push(path.clone()),
            Outcome::NotApplicable if listed => mismatches.not_applicable.push(path.clone()),
            _ => {}
        }
    }
    mismatches.unknown = expected
        .iter()
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
    fn a_run_matching_its_list_has_no_mismatches() {
        let expected = set(&["a/fails.kt"]);
        let run = outcomes(&[
            ("a/fails.kt", Outcome::Fail),
            ("a/passes.kt", Outcome::Pass),
        ]);
        let corpus = set(&["a/fails.kt", "a/passes.kt"]);
        assert!(compare(&expected, &run, &corpus).is_empty());
    }

    #[test]
    fn an_unlisted_failure_and_a_listed_pass_both_mismatch() {
        let expected = set(&["fixed.kt"]);
        let run = outcomes(&[("fixed.kt", Outcome::Pass), ("broken.kt", Outcome::Fail)]);
        let corpus = set(&["fixed.kt", "broken.kt"]);
        let mismatches = compare(&expected, &run, &corpus);
        assert_eq!(mismatches.unexpected_failures, ["broken.kt"]);
        assert_eq!(mismatches.unexpected_passes, ["fixed.kt"]);
    }

    #[test]
    fn stale_entries_mismatch() {
        let expected = set(&["js_only.kt", "deleted.kt"]);
        let run = outcomes(&[("js_only.kt", Outcome::NotApplicable)]);
        let corpus = set(&["js_only.kt"]);
        let mismatches = compare(&expected, &run, &corpus);
        assert_eq!(mismatches.not_applicable, ["js_only.kt"]);
        assert_eq!(mismatches.unknown, ["deleted.kt"]);
    }

    #[test]
    fn a_listed_file_outside_this_shard_is_not_judged() {
        let expected = set(&["other_shard.kt"]);
        let run = outcomes(&[("mine.kt", Outcome::Pass)]);
        let corpus = set(&["mine.kt", "other_shard.kt"]);
        assert!(compare(&expected, &run, &corpus).is_empty());
    }

    #[test]
    fn a_rendered_list_parses_back_to_its_paths() {
        let failing = set(&["b/two.kt", "a/one.kt"]);
        let text = render(KotlinVersion::V2_4_10, &failing);
        assert!(text.starts_with("# Kotlin 2.4.10 "));
        assert!(text.ends_with("a/one.kt\nb/two.kt\n"));
        assert_eq!(parse(&text), failing);
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
