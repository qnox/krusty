//! Scope functions with a receiver-function value argument (`x.apply(block)`), not only literal
//! receiver lambdas (`x.apply { ... }`). The stdlib `apply` body is private `@InlineOnly`, so resolution
//! must accept the function value and the backend must splice the real body.

use super::common;

#[test]
fn apply_accepts_receiver_function_value_argument() {
    const SRC: &str = "// WITH_STDLIB\n\
class Buildee<T> {\n\
    var out: String = \"\"\n\
    fun yield(arg: T) { out = arg.toString() }\n\
}\n\
fun <T> build(instructions: Buildee<T>.() -> Unit): Buildee<T> {\n\
    return Buildee<T>().apply(instructions)\n\
}\n\
fun box(): String {\n\
    val b = build<String> { yield(\"OK\") }\n\
    return b.out\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "ScopeValueArg");
}

const CAPTURED_SRC: &str = "// WITH_STDLIB\n\
interface Sink { fun accept(value: Throwable?) }\n\
fun captured(block: (Throwable) -> Unit): Sink = object : Sink {\n\
    override fun accept(value: Throwable?) { value?.let(block) }\n\
}\n\
fun box(): String {\n\
    var result = \"FAIL\"\n\
    captured { result = it.message!! }.accept(RuntimeException(\"OK\"))\n\
    return result\n\
}\n";

#[test]
fn let_accepts_a_function_value_captured_by_an_object() {
    common::expect_box_ok_with_stdlib(CAPTURED_SRC, "ScopeCapturedValueArg");
}

/// The capture field is read once and invoked, exactly as kotlinc inlines `let` over a non-literal
/// function value: for a plain capture field and for the shared cell of a captured `var`.
#[test]
fn let_of_a_captured_function_value_matches_kotlinc_bytes() {
    const SRC: &str = "interface Sink { fun accept(value: String): Int }\n\
fun captured(block: (String) -> Int): Sink = object : Sink {\n\
    override fun accept(value: String): Int = value.let(block)\n\
}\n\
fun shared(start: (String) -> Int): Sink {\n\
    var block = start\n\
    val sink = object : Sink {\n\
        override fun accept(value: String): Int = value.let(block)\n\
    }\n\
    block = { it.length * 2 }\n\
    return sink\n\
}\n";
    for class in [
        "ScopeCapturedValueBytesKt$captured$1",
        "ScopeCapturedValueBytesKt$shared$sink$1",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "ScopeCapturedValueBytes",
            SRC,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|difference| panic!("{difference}"));
    }
}

const EXPRESSION_VALUES_SRC: &str = "class Holder(val block: (String) -> String)\n\
fun identity(value: String): String = value\n\
fun choose(flag: Boolean, first: (String) -> String, second: (String) -> String) =\n\
    if (flag) first else second\n\
fun condition(block: (String) -> Boolean): String =\n\
    if (\"x\".let(block)) \"\" else \"FAIL\"\n\
fun box(): String {\n\
    val fromProperty = \"O\".let(Holder { it }.block)\n\
    val fromConditional = \"K\".let(choose(true, { it }, { \"FAIL\" }))\n\
    val fromReference = \"\".let(::identity)\n\
    val fromCondition = condition { it == \"x\" }\n\
    return fromProperty + fromConditional + fromReference + fromCondition\n\
}\n";

#[test]
fn let_invokes_property_computed_and_callable_reference_values() {
    common::expect_box_ok_with_stdlib(EXPRESSION_VALUES_SRC, "ScopeExpressionValueArg");
}

#[test]
fn let_of_property_computed_and_callable_reference_values_matches_kotlinc_bytes() {
    common::byte_diff_against_kotlinc_cp(
        "ScopeExpressionValueArgBytes",
        EXPRESSION_VALUES_SRC,
        "ScopeExpressionValueArgBytesKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|difference| panic!("{difference}"));
}
