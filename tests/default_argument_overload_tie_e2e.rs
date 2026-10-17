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

/// A family the selector cannot decide keeps the lambda unshaped. The direct `(Int) -> Unit`
/// overload selects on its own, but the defaulted-slot family's `(String) -> Unit` and
/// `(Boolean) -> Unit` overloads tie, so the call is ambiguous rather than committed to `Int`.
#[test]
fn an_undecided_family_keeps_the_call_ambiguous() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "Main.kt",
            "fun pick(block: (Int) -> Unit) = \"int\"\n\
             fun pick(n: Int = 0, block: (String) -> Unit) = \"string\"\n\
             fun pick(m: Long = 0, block: (Boolean) -> Unit) = \"boolean\"\n\
             fun shaped(): String = pick { it + 1 }\n",
        )],
        &[],
    );
}

/// A lambda whose explicit parameter type rules the other families out selects its own.
#[test]
fn an_explicit_parameter_type_selects_its_family() {
    common::expect_box_same_as_kotlinc(
        "fun pick(block: (Int) -> Unit) = \"int\"\n\
         fun pick(n: Int = 0, block: (String) -> Unit) = \"string\"\n\
         fun pick(m: Long = 0, block: (Boolean) -> Unit) = \"boolean\"\n\
         fun box(): String = if (pick { x: String -> x.length } == \"string\") \"OK\" else \"fail\"\n",
        "UndecidedOverloadFamily",
    );
}
