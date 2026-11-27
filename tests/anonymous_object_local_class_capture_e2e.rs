//! An anonymous object that constructs a local class carries what that class captures.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

const BOTH: &str =
    include_str!("box_regressions/anonymousObjectCapturesBothShadowedAndHiddenValues.kt");
const SHADOWED: &str =
    include_str!("box_regressions/anonymousObjectConstructsCapturingLocalClassShadowed.kt");
const PROGRAM: &str =
    include_str!("box_regressions/anonymousObjectConstructsCapturingLocalClass.kt");

#[test]
fn an_anonymous_object_constructing_a_capturing_local_class_runs_on_the_jvm() {
    expect_box_ok_with_stdlib(PROGRAM, "AnonymousObjectConstructsCapturingLocalClass");
}

#[test]
fn an_anonymous_object_constructing_a_capturing_local_class_runs_on_native() {
    // The JVM/kotlinc run establishes the expected result before Native is held to it.
    expect_box_ok_with_stdlib(PROGRAM, "AnonymousObjectConstructsCapturingLocalClass");
    expect_native_box(
        PROGRAM,
        "AnonymousObjectConstructsCapturingLocalClass",
        "OK",
    );
}

#[test]
fn a_nearer_variable_of_the_same_name_does_not_hide_the_local_class_capture() {
    expect_box_ok_with_stdlib(SHADOWED, "AnonymousObjectShadowedCapture");
    expect_native_box(SHADOWED, "AnonymousObjectShadowedCapture", "OK");
}

#[test]
fn an_object_captures_both_the_nearer_and_the_hidden_variable_of_one_name() {
    expect_box_ok_with_stdlib(BOTH, "AnonymousObjectBothCaptures");
    expect_native_box(BOTH, "AnonymousObjectBothCaptures", "OK");
}
