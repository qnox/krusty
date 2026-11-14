//! A function value over an unsigned array class (`UIntArray`) and a reference to an unsigned array
//! factory (`::uintArrayOf`). The class and its signed carrier share one descriptor, so each
//! boundary — the invocation's erased result, a reference carrier's `invoke` bridge — has to move
//! the box, not the carrier. The unsigned factories are ordinary stdlib functions
//! (`kotlin.UIntArrayKt`), not compiler-provided ones, so a reference to one is a stdlib reference.

use super::common;

#[test]
fn a_lambda_over_an_unsigned_array_returns_its_box() {
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/unsignedArrayLambdaValue.kt"),
        "UnsignedArrayLambda",
    );
}

#[test]
fn a_function_reference_over_an_unsigned_array_unboxes_its_argument_and_boxes_its_result() {
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/unsignedArrayFunctionReferenceValue.kt"),
        "UnsignedArrayReference",
    );
}

#[test]
fn references_to_unsigned_array_factories_run() {
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/unsignedArrayFactoryReferences.kt"),
        "UnsignedArrayFactoryReferences",
    );
}
