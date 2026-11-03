//! PLATFORM: JVM A lambda literal takes the position of the nearest call enclosing it, as
//! kotlinc's `PatchLambdaOffsetsLowering` moves it: the lambda's instance is created on the call's
//! line, not the line the lambda opens on. The call is the nearest one however deep the lambda sits
//! in its arguments (through a constructor call or a conditional), a property setter, an operator
//! `set` call and an array factory included. A variable's initializer and a local function's or
//! inline lambda's body are declarations, under which a lambda keeps its own line, as do an
//! anonymous function and a lambda no call encloses.

use super::common;

const SOURCE: &str = include_str!("fixtures/lambda_argument_call_line/LambdaArgumentCallLine.kt");

#[test]
fn lambda_argument_lines_match_every_class_artifact() {
    let classes = common::classes_against_kotlinc_module(&[("LambdaArgumentCallLine.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn lambda_arguments_keep_their_behavior() {
    common::expect_box_same_as_kotlinc(SOURCE, "LambdaArgumentCallLine");
}
