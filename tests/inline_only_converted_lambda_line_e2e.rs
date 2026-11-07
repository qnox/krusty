//! PLATFORM: JVM An `@InlineOnly` call whose lambda body starts on the call's line separates the
//! two with the synthetic inline line, also when the lambda's result is converted to the expected
//! type (`s.let { it.trim() }` where a `String?` is expected). The conversion has no position of
//! its own; the lambda starts on its value's line.

use super::common;

const SOURCE: &str =
    include_str!("fixtures/inline_only_converted_lambda_line/InlineOnlyConvertedLambdaLine.kt");

#[test]
fn converted_lambda_lines_match_every_class_artifact() {
    let classes =
        common::classes_against_kotlinc_module(&[("InlineOnlyConvertedLambdaLine.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn converted_lambdas_keep_their_behavior() {
    common::expect_box_same_as_kotlinc(SOURCE, "InlineOnlyConvertedLambdaLine");
}
