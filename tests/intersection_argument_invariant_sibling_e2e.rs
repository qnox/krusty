//! A smart-cast type-parameter value passed beside an argument that fixes the formal exactly.

use super::common;

#[test]
fn an_intersection_argument_fits_the_formal_an_invariant_sibling_fixes() {
    const SOURCE: &str =
        include_str!("box_regressions/intersectionArgumentFixedByInvariantSibling.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "intersection-argument-invariant-sibling");
}
