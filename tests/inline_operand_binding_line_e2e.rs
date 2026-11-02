//! PLATFORM: JVM An inline call's operands are stored into its parameter slots with no line of
//! their own, as kotlinc's `genValueAndPut` stores them. A receiver that is itself an inlined call
//! on an earlier line (`filter { }` then `forEach { }`) leaves no entry for that line at the store;
//! the call's own line starts at the inlined body's opening.

use super::common;

const SOURCE: &str =
    include_str!("fixtures/inline_operand_binding_line/InlineOperandBindingLine.kt");

#[test]
fn inline_operand_binding_lines_match_every_class_artifact() {
    let classes =
        common::classes_against_kotlinc_module(&[("InlineOperandBindingLine.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn inline_operand_bindings_keep_their_behavior() {
    common::expect_box_same_as_kotlinc(SOURCE, "InlineOperandBindingLine");
}
