//! Complete, backend-owned outcomes of the portable multimodule inventory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use krusty::kotlin_version::KotlinVersion;

use super::box_lane::Outcome;
use super::box_ratchet::{self, Baseline, Platform};
use super::multimodule_conformance_cases::Case;

const DIRECTORY: &str = "multimodule_expected_failures";
const BLESS: &str = "KRUSTY_BLESS_MULTIMODULE_EXPECTATIONS";

#[derive(Debug)]
struct Inventory {
    corpus: BTreeSet<String>,
    actual: BTreeMap<String, Outcome>,
}

impl Inventory {
    fn compare(&self, baseline: &Baseline) -> box_ratchet::Mismatches {
        let outcomes = self
            .actual
            .iter()
            .map(|(stem, outcome)| (stem.clone(), super::box_lane::ratchet_outcome(outcome)))
            .collect();
        box_ratchet::compare(baseline, &outcomes, &self.corpus)
    }
}

fn collect(cases: &[Case], actual: Vec<(String, Outcome)>) -> Result<Inventory, String> {
    if cases.is_empty() {
        return Err("empty multimodule inventory".into());
    }
    let mut corpus = BTreeSet::new();
    for case in cases {
        if !corpus.insert(case.stem.to_string()) {
            return Err(format!("duplicate multimodule case: {}", case.stem));
        }
    }
    let mut observed = BTreeMap::new();
    for (stem, outcome) in actual {
        if !corpus.contains(&stem) {
            return Err(format!("unexpected multimodule outcome: {stem}"));
        }
        if matches!(
            outcome,
            Outcome::CompilerPanic(_) | Outcome::Harness(_) | Outcome::NotApplicable(_)
        ) {
            return Err(format!("fatal multimodule outcome for {stem}: {outcome:?}"));
        }
        if observed.insert(stem.clone(), outcome).is_some() {
            return Err(format!("duplicate multimodule outcome: {stem}"));
        }
    }
    let missing: Vec<_> = corpus
        .iter()
        .filter(|stem| !observed.contains_key(*stem))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "missing multimodule outcomes: {}",
            missing.join(", ")
        ));
    }
    Ok(Inventory {
        corpus,
        actual: observed,
    })
}

fn load(path: &Path) -> Result<Baseline, String> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        format!(
            "failed to read required multimodule expectation {}: {error}",
            path.display()
        )
    })?;
    let failures = box_ratchet::parse(&text).map_err(|error| {
        format!(
            "invalid multimodule expectation {}: {error}",
            path.display()
        )
    })?;
    Ok(Baseline {
        failures,
        not_applicable: BTreeSet::new(),
    })
}

fn bless_requested(value: Option<&str>, ci: bool) -> Result<bool, String> {
    match value {
        None => Ok(false),
        Some("1") if ci => {
            Err("multimodule expectations may only be recorded by a full local run, not CI".into())
        }
        Some("1") => Ok(true),
        Some(value) => Err(format!("{BLESS} must be exactly 1 when set, got {value:?}")),
    }
}

pub(super) fn recording_requested() -> bool {
    let value = match std::env::var(BLESS) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => panic!("invalid {BLESS}: {error}"),
    };
    bless_requested(value.as_deref(), std::env::var_os("CI").is_some())
        .unwrap_or_else(|error| panic!("{error}"))
}

/// Judge complete physical-lane inventories together, before any optional recording can write.
pub(super) fn check(cases: &[Case], lanes: Vec<(Platform, Vec<(String, Outcome)>)>, bless: bool) {
    let version = krusty::kotlin_version::target();
    let mut platforms = BTreeSet::new();
    let mut prepared = Vec::new();
    assert!(!lanes.is_empty(), "no multimodule physical lanes were run");
    for (platform, actual) in lanes {
        assert_ne!(platform, Platform::Jvm, "JVM multimodule cases stay strict");
        assert!(
            platforms.insert(platform.name()),
            "duplicate multimodule physical lane: {}",
            platform.name()
        );
        let inventory =
            collect(cases, actual).unwrap_or_else(|error| panic!("{}: {error}", platform.name()));
        let path = box_ratchet::read_path(DIRECTORY, platform.name(), version);
        let baseline = load(&path).unwrap_or_else(|error| panic!("{error}"));
        let mismatches = inventory.compare(&baseline);
        // Stale inventory entries are invalid even during a full local recording.
        assert!(
            mismatches.unknown.is_empty(),
            "stale multimodule expectations in {}: {:?}",
            path.display(),
            mismatches.unknown
        );
        prepared.push((platform, path, inventory, mismatches));
    }
    let mut problems = Vec::new();
    for (platform, path, inventory, mismatches) in &prepared {
        if !bless && !mismatches.is_empty() {
            let details: Vec<_> = inventory
                .actual
                .iter()
                .filter(|(_, outcome)| !matches!(outcome, Outcome::Pass))
                .map(|(stem, outcome)| format!("{stem}: {outcome:?}"))
                .collect();
            problems.push(format!(
                "{} multimodule outcomes differ from {}:\n{}\n{}",
                platform.name(),
                path.display(),
                mismatches.outcome_changes.join("\n"),
                details.join("\n")
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    if bless {
        for (platform, _, inventory, _) in prepared {
            record(platform, version, &inventory);
        }
    }
}

fn record(platform: Platform, version: KotlinVersion, inventory: &Inventory) {
    let failures: BTreeSet<_> = inventory
        .actual
        .iter()
        .filter(|(_, outcome)| !matches!(outcome, Outcome::Pass))
        .map(|(stem, _)| stem.clone())
        .collect();
    let mut rendered = format!(
        "# Kotlin {version} portable multimodule cases expected to fail on {}.\n\
         # Releases without their own file inherit this record.\n",
        platform.name()
    );
    for stem in &failures {
        rendered.push_str(stem);
        rendered.push('\n');
    }
    box_ratchet::record(DIRECTORY, platform.name(), version, &failures, &rendered)
        .unwrap_or_else(|error| panic!("failed to record multimodule expectations: {error}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::box_ratchet::Platform;
    use krusty::kotlin_version::KotlinVersion;

    const CASES: [Case; 2] = [
        Case {
            stem: "a",
            source: "",
        },
        Case {
            stem: "b",
            source: "",
        },
    ];

    fn outcomes(a: Outcome, b: Outcome) -> Vec<(String, Outcome)> {
        vec![("a".into(), a), ("b".into(), b)]
    }

    #[test]
    fn an_empty_baseline_remains_strict_about_pass_to_fail() {
        let observed = collect(
            &CASES,
            outcomes(Outcome::Pass, Outcome::Declined("form".into())),
        )
        .unwrap();
        assert_eq!(
            observed.compare(&Baseline::default()).outcome_changes,
            ["b: expected pass, got fail"]
        );
        let passing = collect(&CASES, outcomes(Outcome::Pass, Outcome::Pass)).unwrap();
        assert!(passing.compare(&Baseline::default()).is_empty());
    }

    #[test]
    fn a_measured_failure_cannot_silently_pass_or_disappear() {
        let baseline = Baseline {
            failures: ["b".into(), "removed".into()].into_iter().collect(),
            not_applicable: BTreeSet::new(),
        };
        let observed = collect(&CASES, outcomes(Outcome::Pass, Outcome::Pass)).unwrap();
        let mismatches = observed.compare(&baseline);
        assert_eq!(mismatches.outcome_changes, ["b: expected fail, got pass"]);
        assert_eq!(mismatches.unknown, ["removed"]);
    }

    #[test]
    fn fatal_outcomes_are_rejected_before_failure_mapping() {
        for outcome in [
            Outcome::CompilerPanic("panic".into()),
            Outcome::Harness("setup".into()),
            Outcome::NotApplicable("directive"),
        ] {
            let expected = format!("fatal multimodule outcome for b: {outcome:?}");
            assert_eq!(
                collect(&CASES, outcomes(Outcome::Pass, outcome)).unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn coverage_must_be_complete_unique_and_exact() {
        assert_eq!(
            collect(&CASES, vec![("a".into(), Outcome::Pass)]).unwrap_err(),
            "missing multimodule outcomes: b"
        );
        assert_eq!(
            collect(&CASES, vec![("other".into(), Outcome::Pass)]).unwrap_err(),
            "unexpected multimodule outcome: other"
        );
        assert_eq!(
            collect(
                &CASES,
                vec![("a".into(), Outcome::Pass), ("a".into(), Outcome::Pass)]
            )
            .unwrap_err(),
            "duplicate multimodule outcome: a"
        );
        assert_eq!(
            collect(&[CASES[0], CASES[0]], Vec::new()).unwrap_err(),
            "duplicate multimodule case: a"
        );
        assert_eq!(
            collect(&[], Vec::new()).unwrap_err(),
            "empty multimodule inventory"
        );
    }

    #[test]
    fn missing_and_malformed_baselines_are_hard_errors() {
        let scratch = super::super::common::scratch_dir().unwrap();
        let path = scratch.join("baseline.txt");
        assert!(load(&path).is_err());
        box_ratchet::write_atomic(&path, "a\na\n").unwrap();
        assert_eq!(
            load(&path).unwrap_err(),
            format!(
                "invalid multimodule expectation {}: duplicate path on line 2: a",
                path.display()
            )
        );
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn version_records_inherit_without_duplicate_release_files() {
        let scratch = super::super::common::scratch_dir().unwrap();
        let directory = scratch.to_str().unwrap();
        let versions = KotlinVersion::supported();
        let oldest = versions[0];
        let failures = BTreeSet::new();
        box_ratchet::record(
            directory,
            Platform::Native.name(),
            oldest,
            &failures,
            "# empty\n",
        )
        .unwrap();
        for version in versions {
            let path = box_ratchet::read_path(directory, Platform::Native.name(), version);
            assert!(path.ends_with(format!("native-klib/{oldest}.txt")));
            assert_eq!(load(&path).unwrap(), Baseline::default());
        }
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn recording_needs_the_exact_local_opt_in() {
        assert_eq!(bless_requested(None, false), Ok(false));
        assert_eq!(bless_requested(Some("1"), false), Ok(true));
        assert_eq!(
            bless_requested(Some("true"), false),
            Err(
                "KRUSTY_BLESS_MULTIMODULE_EXPECTATIONS must be exactly 1 when set, got \"true\""
                    .into()
            )
        );
        assert_eq!(
            bless_requested(Some("1"), true),
            Err("multimodule expectations may only be recorded by a full local run, not CI".into())
        );
    }
}
