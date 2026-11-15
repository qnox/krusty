//! An inline function's anonymous object whose suspend member calls a `crossinline` lambda,
//! created inside a suspend lambda. The lambda is spliced into the object's member, so its
//! implementation has no body left; the object's copy for the call site still names that consumed
//! implementation, and its copy has no method of its own to emit.
//!
//! kotlinc 2.4 writes classes for this program that fail verification (KT-86095), so the shared
//! box regression is bounded to kotlinc 2.5 and later, where it runs on every backend against
//! kotlinc. This also pins krusty's own JVM run on every release.

use super::common;

const PROGRAM: &str = include_str!("box_regressions/InlineObjectInSuspendLambda.kt");

#[test]
fn an_inline_object_in_a_suspend_lambda_keeps_its_consumed_lambda_consumed() {
    assert_eq!(
        common::expect_box_run_with_stdlib(PROGRAM, "InlineObjectInSuspendLambda"),
        "OK"
    );
}
