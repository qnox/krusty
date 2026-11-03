//! PLATFORM: JVM A local initialized on a later line than its `val` records the initializer's line
//! and then the declaration's at the store, as kotlinc's `visitVariable` does.

use super::common;

const SOURCE: &str = include_str!("fixtures/variable_initializer_line/VariableInitializerLine.kt");

#[test]
fn variable_initializer_lines_match_every_class_artifact() {
    let classes = common::classes_against_kotlinc_module(&[("VariableInitializerLine.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn variable_initializers_keep_their_values() {
    common::expect_box_same_as_kotlinc(SOURCE, "VariableInitializerLine");
}
