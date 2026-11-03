//! PLATFORM: JVM kotlinc inlines a lambda body as a method of its own: the body and its locals
//! end at the lambda's own result, and the conversion to the type its inline function expects
//! (boxing `parse(it)` for `let`'s `R` where an `Int?` or `Any` is expected) follows those
//! locals, before the inline function's line returns.

use super::common;

const SOURCE: &str =
    include_str!("fixtures/lambda_result_conversion_line/LambdaResultConversion.kt");

#[test]
fn lambda_result_conversions_match_every_class_artifact() {
    let classes = common::classes_against_kotlinc_module(&[("LambdaResultConversion.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn lambda_result_conversions_keep_their_behavior() {
    common::expect_box_same_as_kotlinc(SOURCE, "LambdaResultConversion");
}
