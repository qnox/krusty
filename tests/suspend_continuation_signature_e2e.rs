//! A generic suspend function's continuation class carries the function's type parameters, as
//! kotlinc's does: `<T:Ljava/lang/Object;>Lkotlin/coroutines/jvm/internal/ContinuationImpl;`.

use super::common;

const SOURCE: &str = r#"suspend fun next(): Int = 2

suspend fun <T> keep(value: T): T {
    next()
    return value
}

suspend fun <T : CharSequence, U : Comparable<U>> both(text: T, order: U): T {
    next()
    return text
}
"#;

#[test]
fn a_generic_suspend_function_continuation_declares_its_type_parameters_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendContinuationSignature",
        SOURCE,
        &[
            "SuspendContinuationSignatureKt$keep$1",
            "SuspendContinuationSignatureKt$both$1",
        ],
    );
}
