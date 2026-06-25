//! Regression pins for real Kotlin codegen/box-corpus cases — driven through krusty + the JVM via the
//! SAME corpus the differential conformance gate runs over (`common::run_box_corpus_case`), instead of
//! hand-written snippets that keep hitting lowering edges the corpus cases don't. These are the exact
//! cases that the `Ty::Nullable` rework miscompiled (a 57-case `VerifyError` the hand-written e2e
//! missed entirely) — all nullable-primitive boxing / boxed-comparison shapes — now `box()=OK`.
//!
//! Needs the provisioned box corpus (`KRUSTY_KOTLIN_BOX_DIR`) + JVM toolchain; skips otherwise.

mod common;

/// Single-file `// WITH_STDLIB` box cases from the corpus that regressed under the nullable rework and
/// must stay `box()=OK`. (Multi-file `// FILE:` cases and ones needing features krusty skips are left
/// to the full conformance gate.)
const REGRESSED_CASES: &[&str] = &[
    "binaryOp/eqNullableShortToShort.kt",
    "binaryOp/eqNullableToPrimitiveWithSideEffects.kt",
    "boxing/boxing10.kt",
    "boxingOptimization/kt5588.kt",
    "casts/genericReturnCast.kt",
    "constants/constantsInWhen.kt",
    "controlStructures/compareBoxedIntegerToZero.kt",
    "dataClasses/hashCode/null.kt",
    "ieee754/nullableIntEquals.kt",
    "inline/requireNotNull.kt",
];

/// A broader cross-subsystem net of real corpus cases krusty box-OKs — touching the areas the Ty/IR
/// consolidation reworked (a data-class multi-declaration / IrField, a smart-cast, a generic-constant
/// `as`, a `when`, control flow). Curated to box-OK today (the conformance gate proves the full set);
/// pinned here for fast, attributable feedback on a regression.
const SUBSYSTEM_CASES: &[&str] = &[
    "dataClasses/multiDeclarationFor.kt",
    "smartCasts/smartCastInsideIf.kt",
    "casts/asForConstants.kt",
    "when/noElseExhaustiveStatement.kt",
    "controlStructures/kt769.kt",
];

#[test]
fn subsystem_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(SUBSYSTEM_CASES);
}

/// Run each corpus case; a case that RAN must be `box()=OK` (any other value is a real miscompile),
/// while `None` (skipped: declined / multi-file) is tolerated exactly as the conformance gate counts
/// it — so this e2e is never stricter than the gate. Guards against the whole set silently all-skipping.
fn assert_corpus_cases_box_ok(cases: &[&str]) {
    if !common::corpus_ready() {
        return;
    }
    let mut failures = Vec::new();
    let mut ran = 0;
    for &rel in cases {
        if let Some(out) = common::run_box_corpus_case(rel) {
            ran += 1;
            if out != "OK" {
                failures.push(format!("{rel}: {out}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} corpus case(s) miscompiled (not box()=OK):\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        ran > 0,
        "no corpus case ran with the corpus provisioned — classpath/provisioning broken?"
    );
}

#[test]
fn nullable_boxing_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(REGRESSED_CASES);
}
