//! A labeled return from an inlined `Unit` lambda that suspends. The coroutine places the lambda's
//! body into the function's own code, where nothing follows it but the value it leaves for the
//! inline function, so the frame's early exit is bound in the body itself. Placing the exit after a
//! lambda method's return, as a lambda compiled for the inliner does, left the label unbound and
//! the suspension was reported as one inside a lambda that was not spliced.
use super::common;

#[test]
fn a_suspending_unit_lambda_labeled_return_continues_the_inlined_loop() {
    let source = include_str!("box_regressions/suspendLambdaLabeledReturn.kt");
    common::expect_box_ok_with_stdlib(source, "SuspendLambdaLabeledReturn");
}
