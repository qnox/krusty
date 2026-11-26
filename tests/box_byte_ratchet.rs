//! Per-case JVM byte-identity ratchet.
//!
//! A scored box case is byte-identical when the compiled box passes and every `.class` krusty emits
//! equals the pinned reference kotlinc's, byte for byte. The cases that are not identical are
//! committed per Kotlin release under `tests/box_byte_non_identical/<lane>/<version>.txt` as a set
//! of corpus keys, next to the box expectation lists and with the same lookup rules
//! ([`super::box_ratchet::read_path`]): a release with no file of its own reads its predecessor's.
//!
//! The list is exact in both directions. A case that is not identical and not listed has diverged
//! from kotlinc, which is a regression; a listed case that is identical must be removed, so a later
//! regression of that case fails instead of hiding behind a stale row. For every case the base
//! already scored, the lists only shrink (`scripts/check-outcome-lists.sh`). A key new to the JVM
//! lane (an owned regression the base does not have, or a case its JVM not-applicable list
//! excluded) may be listed in the change that adds it.
//!
//! Compiler-plugin corpora run in their own harness (`tests/plugin_box_corpus.rs`), not through
//! this lane, and keep their own inventories beside their descriptors.

use std::collections::BTreeSet;
use std::path::PathBuf;

use krusty::kotlin_version::KotlinVersion;

use super::box_byte_score::ByteScore;
use super::box_ratchet;

const DIRECTORY: &str = "box_byte_non_identical";

/// Whether a passing case's classes equal the reference's byte for byte.
pub fn is_identical(score: ByteScore) -> bool {
    score.matched == score.total
}

/// The inventory `version` reads for `lane`.
pub fn path(lane: &str, version: KotlinVersion) -> PathBuf {
    box_ratchet::read_path(DIRECTORY, lane, version)
}

fn load(lane: &str, version: KotlinVersion) -> BTreeSet<String> {
    let path = path(lane, version);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "required byte-identity inventory {}: {error}",
            path.display()
        )
    });
    box_ratchet::parse(&text).unwrap_or_else(|error| {
        panic!(
            "invalid byte-identity inventory {}: {error}",
            path.display()
        )
    })
}

fn render(lane: &str, version: KotlinVersion, paths: &BTreeSet<String>) -> String {
    let mut text = format!(
        "# Kotlin {version} codegen/box cases whose {lane} .class files are not byte-identical to\n\
         # kotlinc's, relative to compiler/testData/codegen/box. Later releases with no file of their\n\
         # own read this one. Owned fixtures use regressions/<path relative to tests/box_regressions>.\n\
         # Regenerate with KRUSTY_BLESS_BOX_BYTE_IDENTITY=1 on a full local {lane} conformance run.\n\
         # The list only shrinks, except that a case new to the lane may be listed in the change that\n\
         # adds it; a case the base already scored fails instead of being re-listed.\n",
    );
    for path in paths {
        text.push_str(path);
        text.push('\n');
    }
    text
}

/// What differs between the committed inventory and a run's.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Drift {
    /// Not identical, and not listed: byte identity regressed (or a new case diverges).
    pub newly_divergent: BTreeSet<String>,
    /// Listed, but identical now: the row must go.
    pub now_identical: BTreeSet<String>,
}

pub fn compare(expected: &BTreeSet<String>, actual: &BTreeSet<String>) -> Drift {
    Drift {
        newly_divergent: actual.difference(expected).cloned().collect(),
        now_identical: expected.difference(actual).cloned().collect(),
    }
}

impl Drift {
    pub fn is_empty(&self) -> bool {
        self.newly_divergent.is_empty() && self.now_identical.is_empty()
    }

    pub fn report(&self, lane: &str, version: KotlinVersion) -> String {
        self.report_for(&path(lane, version), lane, version)
    }

    /// The report for the list `version` reads. A stale row can be dropped mechanically only from
    /// the release's own list: an inherited list is read by every later release without a file of
    /// its own, so one release's result cannot say the row is stale for all of them.
    fn report_for(&self, list: &std::path::Path, lane: &str, version: KotlinVersion) -> String {
        let own =
            list.file_name().and_then(|name| name.to_str()) == Some(&format!("{version}.txt"));
        let mut text = format!(
            "{lane} byte identity disagrees with {} (Kotlin {version}):\n",
            list.display()
        );
        for case in &self.newly_divergent {
            text.push_str(&format!(
                "  {case}: expected byte-identical, got different bytes\n"
            ));
        }
        for case in &self.now_identical {
            if own {
                text.push_str(&format!(
                    "  {case}: expected different bytes, got byte-identical; remove it from the list\n"
                ));
            } else {
                text.push_str(&format!(
                    "  {case}: expected different bytes, got byte-identical; the list is inherited, so remove it by hand once every release that reads it agrees\n"
                ));
            }
        }
        text
    }
}

/// Enforce the committed inventory against a full run's non-identical cases, or rewrite it when
/// blessing. `non_identical` holds the corpus key of every scored, applicable case whose bytes
/// differ. A narrowed run (`full_run` false) checks only for new divergences.
pub fn enforce(
    lane: &str,
    version: KotlinVersion,
    non_identical: &BTreeSet<String>,
    bless: bool,
    full_run: bool,
) {
    if bless {
        box_ratchet::record(
            DIRECTORY,
            lane,
            version,
            non_identical,
            &render(lane, version, non_identical),
        )
        .unwrap_or_else(|error| panic!("failed to record byte-identity inventory: {error}"));
        eprintln!(
            "{lane} byte identity: recorded {} non-identical cases for Kotlin {version}",
            non_identical.len()
        );
        return;
    }
    let mut drift = compare(&load(lane, version), non_identical);
    if !full_run {
        // A narrowed run saw only part of the corpus: an unseen listed case is not stale.
        drift.now_identical.clear();
    }
    assert!(drift.is_empty(), "{}", drift.report(lane, version));
    eprintln!(
        "{lane} byte identity matches {} ({} non-identical cases)",
        path(lane, version).display(),
        non_identical.len()
    );
}

#[test]
fn a_new_divergence_and_a_stale_row_are_both_drift() {
    let set = |items: &[&str]| {
        items
            .iter()
            .map(|item| item.to_string())
            .collect::<BTreeSet<_>>()
    };
    let drift = compare(&set(&["a", "b"]), &set(&["b", "c"]));
    assert_eq!(drift.newly_divergent, set(&["c"]));
    assert_eq!(drift.now_identical, set(&["a"]));
    assert!(compare(&set(&["a"]), &set(&["a"])).is_empty());
}

#[test]
fn identity_needs_every_byte() {
    assert!(is_identical(ByteScore {
        matched: 4,
        total: 4
    }));
    assert!(!is_identical(ByteScore {
        matched: 3,
        total: 4
    }));
}

#[test]
fn only_a_releases_own_list_invites_the_mechanical_removal() {
    let version = KotlinVersion::parse("2.4.21").expect("a release");
    let drift = compare(
        &["a"].iter().map(|item| item.to_string()).collect(),
        &BTreeSet::new(),
    );

    let own = drift.report_for(
        std::path::Path::new("tests/box_byte_non_identical/jvm/2.4.21.txt"),
        "jvm",
        version,
    );
    let inherited = drift.report_for(
        std::path::Path::new("tests/box_byte_non_identical/jvm/2.4.20.txt"),
        "jvm",
        version,
    );

    assert_eq!(
        own,
        "jvm byte identity disagrees with tests/box_byte_non_identical/jvm/2.4.21.txt (Kotlin 2.4.21):\n  a: expected different bytes, got byte-identical; remove it from the list\n"
    );
    assert_eq!(
        inherited,
        "jvm byte identity disagrees with tests/box_byte_non_identical/jvm/2.4.20.txt (Kotlin 2.4.21):\n  a: expected different bytes, got byte-identical; the list is inherited, so remove it by hand once every release that reads it agrees\n"
    );
}
