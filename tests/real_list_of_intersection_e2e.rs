//! The real `listOf` (a single-element overload beside a vararg one) takes a local whose type is
//! a non-denotable intersection as a `List<Node>` element, whether the intersection comes from the
//! local's initializer or from flow narrowing, for a collection and for a single element. The
//! program runs on every backend from the shared box inventory; this driver compares its result
//! with kotlinc.
use super::common;

const SOURCE: &str = include_str!("box_regressions/LocalIntersectionListOf.kt");

#[test]
fn a_local_intersection_reaches_a_vararg_collection() {
    common::expect_box_same_as_kotlinc(SOURCE, "LocalIntersectionListOfRun");
}
