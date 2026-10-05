//! A suspend lambda that writes a captured `var` matches kotlinc's class byte for byte.
//!
//! The captured `var` is a `Ref$ObjectRef<T>` cell. kotlinc's lambda field carries that generic
//! `Signature`, like the constructor. A suspension result stored into the cell's `element` (an
//! `Object`) goes in without a `checkcast`. The transformer declares the spill fields and the
//! `@DebugMetadata` while it writes `invokeSuspend`, so their pool entries come ahead of the
//! method's code.

use super::common;

const SOURCE: &str = r#"fun builder(block: suspend () -> Unit) {}

suspend fun here(): String = "OK"

fun box(): String {
    var result = ""
    builder {
        result = here()
    }
    return result
}
"#;

#[test]
fn a_suspension_result_stored_into_a_captured_var_matches_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendSharedCapture",
        SOURCE,
        &["SuspendSharedCaptureKt", "SuspendSharedCaptureKt$box$1"],
    );
}

/// Two values live across the suspension: their spill fields lead the pool in declaration order,
/// with entries other members named earlier left in place.
const SPILLED_SOURCE: &str = r#"fun builder(block: suspend () -> Unit) {}

suspend fun here(): String = "OK"

class Box<T>(val v: T)

fun keep(a: Any?, b: Any?) {}

fun box(): String {
    val boxes = Box("O")
    var result = ""
    builder {
        val first = boxes.v
        val second = boxes
        result = here()
        keep(first, second)
    }
    return result
}
"#;

#[test]
fn a_suspend_lambda_spilling_two_values_lays_out_its_pool_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendSpilledCapture",
        SPILLED_SOURCE,
        &["SuspendSpilledCaptureKt", "SuspendSpilledCaptureKt$box$1"],
    );
}
