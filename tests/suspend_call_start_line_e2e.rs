//! PLATFORM: JVM A suspend call marks its own line before its receiver and arguments, as
//! kotlinc's `visitCall` does for every suspend callee so the line precedes the spilling around
//! the call.

use super::common;

const SOURCE: &str = include_str!("fixtures/suspend_call_start_line/SuspendCallStartLine.kt");

#[test]
fn suspend_call_lines_match_every_class_artifact() {
    let classes = common::classes_against_kotlinc_module(&[("SuspendCallStartLine.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn suspend_calls_keep_their_behavior() {
    common::expect_box_same_as_kotlinc(SOURCE, "SuspendCallStartLine");
}
