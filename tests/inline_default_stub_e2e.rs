//! An `inline` function's `$default` stub contains the function body. A default lambda is a
//! class, not an `invokedynamic` bootstrap.

use super::common;

#[test]
fn an_inline_default_lambda_is_a_singleton_class() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultLambda",
        "inline fun f(block: () -> Unit = {}) { block() }\n",
        &["InlineDefaultLambdaKt", "InlineDefaultLambdaKt$f$1"],
    );
}

#[test]
fn an_inline_default_lambda_captures_its_parameter() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultCapture",
        "inline fun f(x: Int, block: () -> Int = { x }): Int = block()\n",
        &["InlineDefaultCaptureKt", "InlineDefaultCaptureKt$f$1"],
    );
}

#[test]
fn a_private_inline_default_lambda_stays_out_of_the_public_abi() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultPrivate",
        "private inline fun f(block: () -> Unit = {}) { block() }\n",
        &["InlineDefaultPrivateKt", "InlineDefaultPrivateKt$f$1"],
    );
}
