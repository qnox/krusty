//! A generic overload whose type parameter is bounded by a classifier is more specific than one
//! whose type parameter is free.

use super::common;

#[test]
fn a_bounded_generic_overload_beats_a_free_generic_overload() {
    const SOURCE: &str = include_str!("box_regressions/genericBoundNarrowsOverloadSpecificity.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "generic-bound-overload-specificity");
}
