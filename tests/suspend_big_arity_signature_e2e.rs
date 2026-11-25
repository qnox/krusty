//! A suspend function type past the numbered `Function22` is written as `FunctionN<R>` over its
//! declared result, as kotlinc writes it, not over `Any?`.

use super::common;

const SOURCE: &str = r#"suspend fun big(c: suspend (Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long) -> String): String = "x"

suspend fun bigUnit(c: suspend (Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long, Long) -> Unit) {}
"#;

#[test]
fn a_suspend_big_arity_parameter_is_signed_over_its_declared_result_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendBigAritySignature",
        SOURCE,
        &["SuspendBigAritySignatureKt"],
    );
}
