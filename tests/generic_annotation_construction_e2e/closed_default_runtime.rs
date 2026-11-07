//! Whole declaration-default regressions compare Kotlin-first runtime behavior.

use super::common;

const REARRANGED_DEFAULT: &str =
    include_str!("../box_regressions/annotationRearrangedDefaultArguments.kt");
const SPREAD_DEFAULTS: &str = include_str!("../box_regressions/annotationSpreadDefaults.kt");

#[test]
fn the_original_rearranged_default_executes_like_kotlin_independently_of_artifact_checks() {
    common::expect_box_same_as_kotlinc(REARRANGED_DEFAULT, "RearrangedAnnotationDefault");
}

#[test]
fn the_original_spread_default_executes_like_kotlin_independently_of_artifact_checks() {
    common::expect_box_same_as_kotlinc(SPREAD_DEFAULTS, "SpreadAnnotationDefaults");
}
