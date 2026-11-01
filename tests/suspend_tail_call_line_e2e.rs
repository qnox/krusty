//! PLATFORM: JVM A suspension point in a suspend function's tail position is returned at the
//! function's end, as kotlinc's `TailCallOptimizationLowering` does: the closing line is marked at
//! the `areturn`, after the result (`Unit` in a `Unit` function), and a bare `return` behind the
//! call is unreachable. Tail positions run through blocks and `when` branches.

use super::common;

const SOURCE: &str = include_str!("fixtures/suspend_tail_call_line/SuspendTailCallLine.kt");

#[test]
fn suspend_tail_call_lines_match_every_class_artifact() {
    let classes = common::classes_against_kotlinc_module(&[("SuspendTailCallLine.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn suspend_tail_calls_keep_their_results() {
    common::expect_box_same_as_kotlinc(SOURCE, "SuspendTailCallLine");
}
