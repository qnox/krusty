//! An inline function's anonymous object, built inside a suspend lambda, may keep a lambda
//! argument of the call as a field. The suspend lambda class must not try to emit that lambda as
//! a method of its own: the inliner already consumed its body.

use super::common;

const CROSSINLINE_LAMBDA_IN_SUSPEND_LAMBDA: &str =
    include_str!("box_regressions/suspendLambdaRetainedInlineObject.kt");

const TWO_LAMBDAS_IN_SUSPEND_LAMBDA: &str =
    include_str!("box_regressions/suspendLambdaRetainedInlineObjectCaptures.kt");

#[test]
fn a_lambda_kept_by_an_inlined_object_does_not_become_a_method_of_the_suspend_lambda() {
    common::expect_box_ok_with_stdlib(CROSSINLINE_LAMBDA_IN_SUSPEND_LAMBDA, "SuspendInlineObject");
}

#[test]
fn lambdas_capturing_values_of_the_suspend_lambda_are_kept_by_the_inlined_object() {
    common::expect_box_ok_with_stdlib(TWO_LAMBDAS_IN_SUSPEND_LAMBDA, "SuspendInlineObjectCaptures");
}
