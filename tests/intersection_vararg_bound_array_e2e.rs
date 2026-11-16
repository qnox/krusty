//! A vararg whose element type was inferred to an intersection. kotlinc approximates the type
//! argument before IR: to the type parameter's declared bound, or to the components' common
//! supertype when the bound is the implicit `Any?`. The array it builds is therefore the one the
//! callee declares (`Zed[]`, `Number[]`) and never the `Object[]` an intersection erases to on its
//! own. The program runs on every backend from the shared box inventory; this driver also compares
//! its class bytes with kotlinc.
use super::common;

const SOURCE: &str = include_str!("box_regressions/IntersectionVarargBoundArray.kt");

#[test]
fn an_intersection_vararg_builds_its_approximated_array() {
    common::assert_class_matches_kotlinc(
        "IntersectionVarargBoundArray",
        SOURCE,
        "IntersectionVarargBoundArrayKt",
    );
    common::expect_box_same_as_kotlinc(SOURCE, "IntersectionVarargBoundArrayRun");
}
