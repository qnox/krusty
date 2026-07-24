//! A non-inline generic call whose return type parameter is inferred from a primitive argument
//! (`fun <T> fizz(x: T): T; fizz(1)`) is a plain `Int` in kotlinc — usable at an `Int` parameter,
//! in arithmetic, and as an `Int` initializer. krusty typed the recovered return as the boxed
//! `Int?` (the physical erased value), so every such use failed with "type mismatch: inferred type
//! is Int but Int was expected". The checker now types it as the non-null primitive and the
//! lowerer's erased-return coercion unboxes the `Object` result. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn inferred_primitive_return_flows_to_primitive_params() {
    // The inlineEvaluationOrder/argumentOfCall.kt shape (side-effect order preserved).
    const SRC: &str = "var log = \"\"\n\
fun <T> fizz(x: T): T { log += \"fizz($x);\"; return x }\n\
fun sum(x: Int, y: Int): Int { log += \"sum($x,$y);\"; return x + y }\n\
fun box(): String {\n\
    val r = sum(fizz(1), fizz(2))\n\
    if (r != 3) return \"FAIL r: $r\"\n\
    return if (log == \"fizz(1);fizz(2);sum(1,2);\") \"OK\" else \"FAIL: $log\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("inferred generic primitive return at Int params compiles + runs"),
        "OK"
    );
}

#[test]
fn inferred_primitive_return_initializes_a_typed_val() {
    const SRC: &str = "fun <T> fizz(x: T): T = x\n\
fun box(): String {\n\
    val r: Int = fizz(41) + 1\n\
    return if (r == 42) \"OK\" else \"FAIL: $r\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("inferred generic primitive return as Int val compiles + runs"),
        "OK"
    );
}

#[test]
fn inferred_primitive_return_works_for_long_and_double() {
    const SRC: &str = "fun <T> pick(x: T): T = x\n\
fun box(): String {\n\
    val l: Long = pick(40L) + 2L\n\
    val d: Double = pick(1.5) * 2.0\n\
    if (l != 42L) return \"FAIL l: $l\"\n\
    return if (d == 3.0) \"OK\" else \"FAIL d: $d\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("inferred generic Long/Double return compiles + runs"),
        "OK"
    );
}

#[test]
fn inferred_primitive_return_boxes_back_into_a_generic_use() {
    // The unboxed result flowing back into a reference/generic context must re-box.
    const SRC: &str = "fun <T> fizz(x: T): T = x\n\
fun show(a: Any): String = a.toString()\n\
fun box(): String = if (show(fizz(7)) == \"7\") \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("inferred generic primitive return re-boxes compiles + runs"),
        "OK"
    );
}

#[test]
fn inferred_primitive_result_stored_and_reused() {
    // The unbox is one-shot at the call site; a stored result must serve BOTH a primitive use and a
    // reference use (re-boxed at the `Any` argument).
    const SRC: &str = "fun <T> fizz(x: T): T = x\n\
fun show(a: Any): String = a.toString()\n\
fun box(): String {\n\
    val x = fizz(20)\n\
    val y = x + 1\n\
    val s = show(x)\n\
    return if (y == 21 && s == \"20\") \"OK\" else \"FAIL: $y $s\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("stored inferred generic primitive result compiles + runs"),
        "OK"
    );
}

#[test]
fn lambda_only_binding_types_the_primitive() {
    // `T` bound ONLY from a lambda's return (no plain-value witness): `supply { 41 }` is an `Int`.
    const SRC: &str = "fun <T> supply(block: () -> T): T = block()\n\
fun box(): String {\n\
    val n = supply { 41 } + 1\n\
    return if (n == 42) \"OK\" else \"FAIL: $n\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("lambda-only generic primitive binding compiles + runs"),
        "OK"
    );
}

#[test]
fn corpus_inline_evaluation_order_argument_of_new() {
    if let Some(out) = common::run_box_corpus_case("inlineEvaluationOrder/argumentOfNew.kt") {
        assert_eq!(out, "OK");
    }
}
