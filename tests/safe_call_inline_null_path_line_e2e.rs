//! PLATFORM: JVM The null path of a safe call whose selector is an inlined call writes the safe
//! call's line again, even when the selector's spliced frame closes after an operand stack restore
//! (a safe call in a call's argument list). kotlinc closes that frame inside the inlined call and
//! only then resets the line in effect (`markLineNumberAfterInlineIfNeeded`).

use super::common;

const SOURCE: &str =
    include_str!("fixtures/safe_call_inline_null_path_line/SafeCallInlineNullPathLine.kt");

#[test]
fn safe_call_null_path_lines_match_every_class_artifact() {
    let classes =
        common::classes_against_kotlinc_module(&[("SafeCallInlineNullPathLine.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn safe_call_null_paths_keep_their_behavior() {
    common::expect_box_same_as_kotlinc(SOURCE, "SafeCallInlineNullPathLine");
}
