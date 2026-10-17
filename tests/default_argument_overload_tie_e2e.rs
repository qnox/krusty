//! Two overloads that pass a trailing lambda to the same function type, one of them only after a
//! defaulted leading parameter, are equally specific; Kotlin prefers the one whose call uses no
//! default argument. The signature pass shapes the lambda before selection is final, and an
//! expression-bodied caller's result type depends on that selection.

use super::common;

#[test]
fn a_trailing_lambda_prefers_the_overload_without_a_defaulted_argument() {
    common::expect_box_same_as_kotlinc(
        r#"
fun app(block: () -> Unit): String { block(); return "OK" }
fun app(n: Int = 0, block: () -> Unit): Int { block(); return n }
fun viaExpressionBody() = app { }

class Builder { var label = "" }
fun build(block: suspend Builder.() -> Unit): String = "OK"
fun build(tag: String = "default", block: suspend Builder.() -> Unit): Int = tag.length
fun viaReceiverLambda() = build { label = "x" }

fun box(): String {
    val plain: String = viaExpressionBody()
    val receiver: String = viaReceiverLambda()
    return if (plain == "OK" && receiver == "OK") "OK" else "$plain $receiver"
}
"#,
        "DefaultArgumentOverloadTie",
    );
}
