//! A function-reference class's `invoke` carries the generic `Signature` kotlinc writes for it.

use super::common;

const SOURCE: &str = r#"fun <T> id(x: T): T = x
fun <T> String.extId(x: T): T = x

fun <T, R> T.myLet(block: (T) -> R): R = block(this)

fun <T> foo(value: T?): T? = value?.myLet(::id)
fun <T> bar(value: T?): T? = value?.myLet(""::extId)
"#;

#[test]
fn a_generic_function_reference_invoke_is_signed_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "FunctionReferenceInvokeSignature",
        SOURCE,
        &[
            "FunctionReferenceInvokeSignatureKt$foo$1",
            "FunctionReferenceInvokeSignatureKt$bar$1",
        ],
    );
}
