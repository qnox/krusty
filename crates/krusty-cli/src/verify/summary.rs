//! What a build's verify records add up to.

use std::collections::BTreeMap;

use krusty::jvm::class_comparison::DifferenceKind;

use super::record::{CompilationRecord, Outcome};

/// Where the records were made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    pub krusty_version: String,
    pub build_tool: Option<String>,
    pub jdk: Option<String>,
    /// `linux-x86_64` and the like.
    pub platform: String,
}

/// The totals over every record of a build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub compilations: usize,
    /// Compilations krusty compiled, identical or not.
    pub compiled: usize,
    /// Compiled compilations whose every output file is kotlinc's.
    pub identical_compilations: usize,
    pub failed: usize,
    pub unsupported: usize,
    /// kotlinc's classes in the compiled compilations.
    pub classes: usize,
    pub identical_classes: usize,
    /// Classes, and other output files, by the primary way they differ: a class kind identifier,
    /// `missing`, `extra`, `kotlin-module` or `other-file`.
    pub differences: BTreeMap<String, usize>,
    /// Over the compiled compilations kotlinc also has a time for.
    pub kotlinc_millis: u64,
    pub krusty_millis: u64,
    /// The largest peak among the compiled compilations.
    pub krusty_peak_rss_bytes: Option<u64>,
    /// The Kotlin versions the compilations used.
    pub kotlin_versions: Vec<String>,
}

impl Summary {
    pub fn of(records: &[CompilationRecord]) -> Summary {
        let mut summary = Summary {
            compilations: records.len(),
            ..Summary::default()
        };
        for record in records {
            if let Some(version) = &record.kotlin_version {
                if !summary.kotlin_versions.contains(version) {
                    summary.kotlin_versions.push(version.clone());
                }
            }
            match &record.outcome {
                Outcome::Unsupported { .. } => summary.unsupported += 1,
                Outcome::Failed { .. } => summary.failed += 1,
                Outcome::Compiled { comparison } => {
                    summary.compiled += 1;
                    if comparison.is_identical() {
                        summary.identical_compilations += 1;
                    }
                    summary.classes += comparison.classes;
                    summary.identical_classes += comparison.identical_classes;
                    let mut count = |kind: &str, by: usize| {
                        if by > 0 {
                            *summary.differences.entry(kind.to_string()).or_default() += by;
                        }
                    };
                    count("missing", comparison.missing.len());
                    count("extra", comparison.extra.len());
                    for difference in &comparison.differences {
                        count(&difference.kind, 1);
                    }
                    if let (Some(kotlinc), Some(krusty)) =
                        (record.kotlinc_millis, record.krusty_millis)
                    {
                        summary.kotlinc_millis += kotlinc;
                        summary.krusty_millis += krusty;
                    }
                    summary.krusty_peak_rss_bytes = summary
                        .krusty_peak_rss_bytes
                        .max(record.krusty_peak_rss_bytes);
                }
            }
        }
        summary.kotlin_versions.sort();
        summary
    }

    /// The differences, most frequent first, then in kind precedence.
    pub fn ranked_differences(&self) -> Vec<(&str, usize)> {
        let mut ranked = self
            .differences
            .iter()
            .map(|(kind, count)| (kind.as_str(), *count))
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(kind, count)| (std::cmp::Reverse(*count), kind_rank(kind)));
        ranked
    }
}

fn kind_rank(kind: &str) -> usize {
    match kind {
        "missing" => 0,
        "extra" => 1,
        _ => DifferenceKind::from_id(kind).map_or(DifferenceKind::ALL.len() + 2, |kind| {
            DifferenceKind::ALL
                .iter()
                .position(|candidate| *candidate == kind)
                .expect("ALL lists every kind")
                + 2
        }),
    }
}

/// One line saying how krusty did on a recorded compilation.
pub fn record_line(record: &CompilationRecord) -> String {
    let outcome = match &record.outcome {
        Outcome::Unsupported { reasons } => {
            format!("unsupported, left to kotlinc: {}", reasons.join("; "))
        }
        Outcome::Failed { errors } => format!("krusty failed with {} error(s)", errors.len()),
        Outcome::Compiled { comparison } => format!(
            "{} of {} classes byte-identical{}",
            comparison.identical_classes,
            comparison.classes,
            if comparison.is_identical() {
                ", output identical"
            } else {
                ""
            }
        ),
    };
    let timing = match (record.krusty_millis, record.krusty_peak_rss_bytes) {
        (Some(millis), Some(peak)) => format!(" ({}, {} peak)", seconds(millis), megabytes(peak)),
        (Some(millis), None) => format!(" ({})", seconds(millis)),
        _ => String::new(),
    };
    format!("krusty verify: {}: {outcome}{timing}", record.compilation)
}

/// One line saying how krusty did on a whole build.
pub fn summary_line(summary: &Summary) -> String {
    format!(
        "krusty verify: {}/{} compilations compiled ({} byte-identical), {} failed, {} unsupported; {} of classes byte-identical",
        summary.compiled,
        summary.compilations,
        summary.identical_compilations,
        summary.failed,
        summary.unsupported,
        percent(summary.identical_classes, summary.classes),
    )
}

/// What a difference kind identifier means, for a reader.
pub fn describe_kind(kind: &str) -> &'static str {
    match kind {
        "missing" => "files kotlinc writes and krusty does not",
        "extra" => "files krusty writes and kotlinc does not",
        "kotlin-module" => "the module's .kotlin_module file",
        "other-file" => "another output file",
        _ => DifferenceKind::from_id(kind)
            .map_or("an unknown difference", DifferenceKind::description),
    }
}

/// `part / whole` as a percentage with one decimal, or `n/a` for nothing.
pub fn percent(part: usize, whole: usize) -> String {
    if whole == 0 {
        "n/a".to_string()
    } else {
        format!("{:.1}%", part as f64 * 100.0 / whole as f64)
    }
}

pub fn seconds(millis: u64) -> String {
    format!("{:.1} s", millis as f64 / 1000.0)
}

pub fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes / (1024 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::outputs::{FileDifference, OutputComparison};

    fn record(outcome: Outcome) -> CompilationRecord {
        CompilationRecord {
            compilation: "c".to_string(),
            source_set: None,
            kotlin_version: Some("2.4.20".to_string()),
            outcome,
            kotlinc_millis: Some(3000),
            krusty_millis: Some(1000),
            krusty_peak_rss_bytes: Some(100),
        }
    }

    #[test]
    fn records_add_up_by_outcome_and_difference() {
        let difference = |kind: &str| FileDifference {
            path: format!("{kind}.class"),
            kind: kind.to_string(),
            member: None,
        };
        let compiled = record(Outcome::Compiled {
            comparison: OutputComparison {
                classes: 10,
                identical_classes: 7,
                missing: vec!["Gone.class".to_string()],
                extra: vec![],
                differences: vec![difference("line-numbers"), difference("instructions")],
            },
        });
        let identical = record(Outcome::Compiled {
            comparison: OutputComparison {
                classes: 2,
                identical_classes: 2,
                ..OutputComparison::default()
            },
        });
        let summary = Summary::of(&[
            compiled,
            identical,
            record(Outcome::Failed {
                errors: vec!["error".to_string()],
            }),
            record(Outcome::Unsupported { reasons: vec![] }),
        ]);
        assert_eq!(
            summary,
            Summary {
                compilations: 4,
                compiled: 2,
                identical_compilations: 1,
                failed: 1,
                unsupported: 1,
                classes: 12,
                identical_classes: 9,
                differences: BTreeMap::from([
                    ("instructions".to_string(), 1),
                    ("line-numbers".to_string(), 1),
                    ("missing".to_string(), 1),
                ]),
                kotlinc_millis: 6000,
                krusty_millis: 2000,
                krusty_peak_rss_bytes: Some(100),
                kotlin_versions: vec!["2.4.20".to_string()],
            }
        );
        assert_eq!(
            summary.ranked_differences(),
            vec![("missing", 1), ("instructions", 1), ("line-numbers", 1)]
        );
    }

    #[test]
    fn percentages_of_nothing_are_not_applicable() {
        assert_eq!(percent(1, 3), "33.3%");
        assert_eq!(percent(0, 0), "n/a");
    }
}
