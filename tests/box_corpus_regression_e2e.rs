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

#[test]
fn nullable_boxing_corpus_cases_box_ok() {
    if !common::corpus_ready() {
        return;
    }
    // `None` = the case was SKIPPED (declined/multi-file) — the conformance gate counts those as skips
    // too, so we don't fail on them (avoids the e2e being stricter than the gate it mirrors). A case
    // that actually RAN must return "OK"; any other value is a real miscompile (the regression class
    // these pins exist for — the 57 were VerifyErrors / wrong box() values, not skips).
    let mut failures = Vec::new();
    let mut ran = 0;
    for &rel in REGRESSED_CASES {
        if let Some(out) = common::run_box_corpus_case(rel) {
            ran += 1;
            if out != "OK" {
                failures.push(format!("{rel}: {out}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} corpus regression case(s) miscompiled (not box()=OK):\n{}",
        failures.len(),
        failures.join("\n")
    );
    // With the corpus provisioned, the set must not silently all-skip (a broken classpath/provisioning
    // would make every case decline to compile and the assert above pass vacuously).
    assert!(
        ran > 0,
        "no corpus regression case ran with the corpus provisioned — classpath/provisioning broken?"
    );
}
