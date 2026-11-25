//! The `invoke` of a lambda class written in a generic declaration carries the generic `Signature`
//! kotlinc writes for it, `()TT;` for a lambda returning the declaration's type parameter.

use super::common;

const SOURCE: &str = r#"inline fun <T> later(crossinline produce: () -> T): () -> T = { produce() }
"#;

#[test]
fn a_lambda_invoke_over_a_type_parameter_is_signed_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "LambdaInvokeSignature",
        SOURCE,
        &["LambdaInvokeSignatureKt$later$1"],
    );
}
