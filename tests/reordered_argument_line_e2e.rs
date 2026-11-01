//! PLATFORM: JVM A call whose named arguments are written out of parameter order stores each
//! argument in a temporary built at the argument's position, as kotlinc does: the store and the
//! read that passes it record the argument's line, through an implicit coercion to its parameter
//! and after an inlined argument.

use super::common;

const SOURCE: &str = include_str!("fixtures/reordered_argument_line/ReorderedArgumentLine.kt");

#[test]
fn reordered_argument_lines_match_every_class_artifact() {
    let classes = common::classes_against_kotlinc_module(&[("ReorderedArgumentLine.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn reordered_arguments_keep_their_values() {
    common::expect_box_same_as_kotlinc(SOURCE, "ReorderedArgumentLine");
}
