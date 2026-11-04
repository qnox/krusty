//! PLATFORM: JVM call and return LineNumberTable positions match complete kotlinc artifacts.

use super::common;

const INLINE_OPERAND: &str = include_str!("fixtures/generic_call_debug/InlineOperandReturn.kt");

#[test]
fn generic_calls_with_inline_operands_match_every_class_artifact() {
    let classes =
        common::classes_against_kotlinc_module(&[("InlineOperandReturn.kt", INLINE_OPERAND)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn generic_call_result_coercions_retain_execution_and_identity() {
    common::expect_box_same_as_kotlinc(INLINE_OPERAND, "InlineOperandReturn");
}
