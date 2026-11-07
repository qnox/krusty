//! PLATFORM: JVM An enum entry's construction marks the entry's line, lets its arguments mark the
//! lines they sit on, and marks the entry's line again at the constructor call, as kotlinc's
//! `visitConstructorCall` does for the call `EnumClassLowering` builds at the entry.

use super::common;

const SOURCE: &str = include_str!("fixtures/enum_entry_argument_lines/EnumEntryArgumentLines.kt");

#[test]
fn enum_entry_argument_lines_match_every_class_artifact() {
    let classes = common::classes_against_kotlinc_module(&[("EnumEntryArgumentLines.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn enum_entries_keep_their_arguments() {
    common::expect_box_same_as_kotlinc(SOURCE, "EnumEntryArgumentLines");
}
