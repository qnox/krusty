//! Byte-prefix equality scoring for the Kotlin box conformance metric.
//!
//! The score grades krusty's emitted `.class` bytes against the pinned reference `kotlinc`'s. Each
//! class is paired by `(module identity, JVM internal class name)` — never by bare name, so a class
//! that repeats across two modules stays two distinct, separately scored artifacts.
//!
//! For a passing box case a paired class contributes its common leading-prefix byte count as the
//! numerator and `max(krusty length, kotlinc length)` as the denominator: identical bytes after the
//! first difference never resume credit. A class present on only one side contributes zero matched
//! bytes and its entire length. A failed box case contributes zero matched bytes against the sum of
//! the reference `.class` sizes, with no byte comparison. A non-applicable case contributes nothing,
//! and a reference-compilation failure makes the whole run fail closed rather than scoring the case.
//!
//! All sums stay integers; the display percentage is derived only at the final reporting boundary.

use std::collections::{BTreeMap, BTreeSet};

use super::box_reference_classes::ReferenceClasses;

/// A `.class` inventory keyed by `(module identity, JVM internal class name)`.
pub type QualifiedClasses = BTreeMap<(String, String), Vec<u8>>;

/// Integer matched / total byte counts for one case or an accumulated set. `matched <= total`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ByteScore {
    pub matched: u64,
    pub total: u64,
}

impl ByteScore {
    pub fn add(&mut self, other: ByteScore) {
        self.matched += other.matched;
        self.total += other.total;
    }
}

/// One applicable case's scoring result during a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaseScore {
    /// Scoring was not active for this run, or the case is not applicable: contributes nothing.
    NotScored,
    /// The case's integer byte score.
    Scored(ByteScore),
    /// The reference compilation failed or was unavailable; the run must fail closed. Carries the
    /// identifiable cause.
    RefFail(String),
}

/// The module-qualified view of a reference inventory, copied into an owned [`QualifiedClasses`].
pub fn qualified_from_reference(reference: &ReferenceClasses) -> QualifiedClasses {
    reference
        .qualified()
        .map(|((module, name), bytes)| ((module.to_string(), name.to_string()), bytes.to_vec()))
        .collect()
}

/// Common leading-prefix byte count and max-length denominator for one module/name-paired class.
///
/// Bytes equal only *after* the first difference do not count: the numerator stops at the first
/// mismatch. Equal classes receive full credit; an unequal-length but equal-prefix pair receives
/// only the shorter length; a first-byte mismatch receives zero.
pub fn prefix_match(krusty: &[u8], reference: &[u8]) -> ByteScore {
    let matched = krusty
        .iter()
        .zip(reference)
        .take_while(|(left, right)| left == right)
        .count() as u64;
    ByteScore {
        matched,
        total: krusty.len().max(reference.len()) as u64,
    }
}

/// Score a passing case over the union of both inventories' `(module, name)` identities.
pub fn score_passing(krusty: &QualifiedClasses, reference: &QualifiedClasses) -> ByteScore {
    let mut score = ByteScore::default();
    let keys: BTreeSet<&(String, String)> = krusty.keys().chain(reference.keys()).collect();
    for key in keys {
        match (krusty.get(key), reference.get(key)) {
            (Some(k), Some(r)) => score.add(prefix_match(k, r)),
            // A class only krusty emitted, or only the reference emitted, is entirely unmatched: no
            // numerator, its whole length in the denominator.
            (Some(k), None) => score.total += k.len() as u64,
            (None, Some(r)) => score.total += r.len() as u64,
            (None, None) => unreachable!("key came from the union of the two maps"),
        }
    }
    score
}

/// Score a failed case: zero matched bytes, denominator is the sum of the reference `.class` sizes.
pub fn score_failed(reference: &QualifiedClasses) -> ByteScore {
    ByteScore {
        matched: 0,
        total: reference.values().map(|bytes| bytes.len() as u64).sum(),
    }
}

/// Decide one applicable case's score from its reference-compilation result and box outcome.
///
/// A reference error — a rejection, a missing input, an unreadable class, or an unavailable
/// compiler — becomes [`CaseScore::RefFail`] so the run fails closed; it is never silently scored as
/// a zero-credit box failure. `krusty` is ignored for a failed box (only the reference denominator
/// is needed), so a case that never produced krusty classes may pass an empty map.
pub fn score_case(
    reference: Result<&QualifiedClasses, &str>,
    box_passed: bool,
    krusty: &QualifiedClasses,
) -> CaseScore {
    match reference {
        Err(cause) => CaseScore::RefFail(cause.to_string()),
        Ok(reference) => {
            let score = if box_passed {
                score_passing(krusty, reference)
            } else {
                score_failed(reference)
            };
            CaseScore::Scored(score)
        }
    }
}

#[cfg(test)]
fn qualified(entries: &[(&str, &str, &[u8])]) -> QualifiedClasses {
    entries
        .iter()
        .map(|(module, name, bytes)| ((module.to_string(), name.to_string()), bytes.to_vec()))
        .collect()
}

#[test]
fn prefix_match_equal_classes_receive_full_credit() {
    assert_eq!(
        prefix_match(&[1, 2, 3, 4], &[1, 2, 3, 4]),
        ByteScore {
            matched: 4,
            total: 4
        }
    );
}

#[test]
fn prefix_match_first_byte_mismatch_receives_zero() {
    assert_eq!(
        prefix_match(&[9, 2, 3], &[1, 2, 3]),
        ByteScore {
            matched: 0,
            total: 3
        }
    );
}

#[test]
fn prefix_match_stops_at_first_difference_despite_equal_suffix() {
    assert_eq!(
        prefix_match(&[1, 2, 3], &[1, 9, 3]),
        ByteScore {
            matched: 1,
            total: 3
        }
    );
}

#[test]
fn prefix_match_unequal_lengths_credit_shorter_prefix_both_directions() {
    assert_eq!(
        prefix_match(&[1, 2], &[1, 2, 3]),
        ByteScore {
            matched: 2,
            total: 3
        }
    );
    // reference shorter: symmetric.
    assert_eq!(
        prefix_match(&[1, 2, 3], &[1, 2]),
        ByteScore {
            matched: 2,
            total: 3
        }
    );
}

#[test]
fn score_passing_sums_paired_classes_by_bytes() {
    let krusty = qualified(&[("main", "A", &[1, 2, 3]), ("main", "B", &[4, 5])]);
    let reference = qualified(&[("main", "A", &[1, 2, 9]), ("main", "B", &[4, 5])]);
    // A has two equal leading bytes; B is entirely byte-identical.
    assert_eq!(
        score_passing(&krusty, &reference),
        ByteScore {
            matched: 4,
            total: 5
        }
    );
}

#[test]
fn score_passing_missing_and_extra_classes_are_entirely_unmatched() {
    let krusty = qualified(&[("main", "A", &[1, 2, 3, 4]), ("main", "Extra", &[7, 7])]);
    let reference = qualified(&[
        ("main", "A", &[1, 2, 3, 4]),
        ("main", "Missing", &[8, 8, 8]),
    ]);
    // A: 4/4 identical. Extra (krusty-only): 0/2. Missing (reference-only): 0/3.
    assert_eq!(
        score_passing(&krusty, &reference),
        ByteScore {
            matched: 4,
            total: 9
        }
    );
}

#[test]
fn score_passing_duplicate_name_in_two_modules_scored_separately() {
    // The same internal name in two modules is two distinct identities: a byte difference in `a`'s
    // copy does not cancel the identical bytes of `b`'s copy, and neither overwrites the other.
    let krusty = qualified(&[("a", "Dup", &[1, 2, 3]), ("b", "Dup", &[1, 2, 3])]);
    let reference = qualified(&[("a", "Dup", &[1, 9, 3]), ("b", "Dup", &[1, 2, 3])]);
    // (a,Dup) shares one leading byte; (b,Dup) is entirely byte-identical.
    assert_eq!(
        score_passing(&krusty, &reference),
        ByteScore {
            matched: 4,
            total: 6
        }
    );
}

#[test]
fn score_passing_zero_denominator_when_both_empty() {
    assert_eq!(
        score_passing(&QualifiedClasses::new(), &QualifiedClasses::new()),
        ByteScore {
            matched: 0,
            total: 0
        }
    );
}

#[test]
fn score_failed_credits_nothing_against_reference_sizes() {
    let reference = qualified(&[("main", "A", &[1, 2, 3]), ("main", "B", &[4, 5, 6, 7])]);
    assert_eq!(
        score_failed(&reference),
        ByteScore {
            matched: 0,
            total: 7
        }
    );
}

#[test]
fn score_failed_zero_denominator_when_reference_empty() {
    assert_eq!(
        score_failed(&QualifiedClasses::new()),
        ByteScore {
            matched: 0,
            total: 0
        }
    );
}

#[test]
fn score_case_passing_counts_the_matching_prefix() {
    let krusty = qualified(&[("main", "A", &[1, 2, 3])]);
    let reference = qualified(&[("main", "A", &[1, 2, 9])]);
    assert_eq!(
        score_case(Ok(&reference), true, &krusty),
        CaseScore::Scored(ByteScore {
            matched: 2,
            total: 3
        })
    );
}

#[test]
fn score_case_failed_box_scores_reference_denominator_without_krusty() {
    let reference = qualified(&[("main", "A", &[1, 2, 3, 4, 5])]);
    // A failed box contributes zero numerator and the reference size, regardless of krusty bytes.
    assert_eq!(
        score_case(Ok(&reference), false, &QualifiedClasses::new()),
        CaseScore::Scored(ByteScore {
            matched: 0,
            total: 5
        })
    );
}

#[test]
fn score_case_reference_error_fails_closed() {
    let krusty = qualified(&[("main", "A", &[1, 2, 3])]);
    assert_eq!(
        score_case(
            Err("reference module main: kt:1:1: error: boom"),
            true,
            &krusty
        ),
        CaseScore::RefFail("reference module main: kt:1:1: error: boom".to_string())
    );
}
