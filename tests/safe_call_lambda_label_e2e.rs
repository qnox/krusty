//! A lambda passed through a safe call takes the callee's name as its implicit label.
//!
//! This is the complete program, run against kotlinc: Unit `return@let` and `return@also` that
//! leave a captured `var` unchanged, and a value `return@let`. The Wasm and Native KLIB lanes
//! cannot realize Unit-valued lambdas or the stdlib string bodies yet, so the box regression
//! `safeCallLambdaImplicitLabel.kt` repeats the label resolution with value-returning lambdas on
//! every target.
use super::common;

const PROGRAM: &str =
    include_str!("fixtures/safe_call_lambda_label/SafeCallLambdaImplicitLabel.kt");

#[test]
fn safe_call_lambdas_take_the_callee_label_like_kotlinc() {
    common::expect_box_same_as_kotlinc(PROGRAM, "SafeCallLambdaImplicitLabel");
}
