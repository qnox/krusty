//! PLATFORM: JVM kotlinc inlines a lambda body as a method of its own, so the conversion of its
//! result (boxing `parse(it)` into a safe call's `Int?`) follows the lambda's locals but precedes
//! the frame's closing line. That line then lands on the safe call's jump past its null path.

use super::common;

const SOURCE: &str =
    include_str!("fixtures/safe_call_lambda_result_conversion/SafeCallLambdaResultConversion.kt");

#[test]
fn safe_call_lambda_result_conversions_match_every_class_artifact() {
    let classes =
        common::classes_against_kotlinc_module(&[("SafeCallLambdaResultConversion.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn safe_call_lambda_result_conversions_keep_their_behavior() {
    common::expect_box_same_as_kotlinc(SOURCE, "SafeCallLambdaResultConversion");
}
