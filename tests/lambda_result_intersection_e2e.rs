//! A lambda whose result is a non-denotable intersection (`if (c) Paragraph() else it`, two classes
//! that share several interfaces) passed to a generic call keeps that intersection as the call's
//! result, so the expected `Pair2<Node>` is checked against its components rather than the `Any`
//! its denotable view collapses to. The program runs on every backend from the shared box
//! inventory; this driver compares its result with kotlinc.
use super::common;

const SOURCE: &str = include_str!("box_regressions/LambdaResultIntersectionArgument.kt");

#[test]
fn a_lambda_result_intersection_reaches_the_generic_call_result() {
    common::expect_box_same_as_kotlinc(SOURCE, "LambdaResultIntersectionArgumentRun");
}
