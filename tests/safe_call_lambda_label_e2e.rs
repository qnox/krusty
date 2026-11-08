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

/// A discarded safe-call `let` whose lambda returns a value through its label and assigns a
/// captured `var`. The labeled return and the fall-through both have to reach the statement after
/// the call with the same operand stack; otherwise frame computation declines with `StackHeight`.
/// The program is the shared regression `discardedSafeCallLetValueReturn.kt`. kotlinc compiles the
/// body and `box()` returns `OK`.
#[test]
fn a_discarded_safe_call_let_with_a_labeled_value_return_computes_frames_like_kotlinc() {
    const SOURCE: &str = include_str!("box_regressions/discardedSafeCallLetValueReturn.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "discarded safe-call let value return");
}
