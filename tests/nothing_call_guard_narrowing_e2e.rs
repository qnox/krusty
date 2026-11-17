//! A guard whose exit is a `Nothing`-returning call narrows what follows it.

use super::common;

#[test]
fn a_guard_that_calls_a_nothing_function_narrows_the_rest_of_the_block() {
    const SOURCE: &str = include_str!("box_regressions/nothingCallGuardNarrowing.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "nothing-call-guard-narrowing");
}
