//! A call-site copy of a lambda written in an inline function keeps the method result the inline
//! function's template erases its type parameter to, and signs the substituted type: kotlinc
//! regenerates the copy from the template class, so `invoke` stays `()Ljava/lang/Enum;` for
//! `E : Enum<E>` and `()Ljava/lang/Object;` for an unbounded parameter, with the argument only in
//! the generic `Signature`.

use super::common;

const SOURCE: &str = r#"enum class Color { RED, GREEN }

inline fun <reified E : Enum<E>> bounded(crossinline produce: () -> E): () -> E = { produce() }

inline fun <reified T> unbounded(crossinline produce: () -> T): () -> T = { produce() }

fun useBounded(): () -> Color = bounded { Color.RED }

fun useUnbounded(): () -> String = unbounded { "x" }
"#;

#[test]
fn a_reified_lambda_copy_keeps_the_template_result_and_signs_the_argument() {
    common::assert_classes_identical_to_kotlinc(
        "LambdaEnumBoundSignature",
        SOURCE,
        &[
            "LambdaEnumBoundSignatureKt$useBounded$$inlined$bounded$1",
            "LambdaEnumBoundSignatureKt$useUnbounded$$inlined$unbounded$1",
        ],
    );
}
