//! PLATFORM: JVM A lambda returning its value as `Any?` marks the return at the value's end line,
//! as kotlinc does, not at the closing brace a `Unit` lambda falls off.

use super::common;

const SOURCE: &str = include_str!("fixtures/nullable_any_lambda_return/NullableAnyLambdaReturn.kt");

#[test]
fn nullable_any_lambda_returns_match_every_class_artifact() {
    let classes = common::classes_against_kotlinc_module(&[("NullableAnyLambdaReturn.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn nullable_any_lambda_returns_keep_their_values() {
    common::expect_box_same_as_kotlinc(SOURCE, "NullableAnyLambdaReturn");
}
