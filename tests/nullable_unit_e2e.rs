//! `Unit?` is a nullable reference to `kotlin/Unit` (values `Unit.INSTANCE` or `null`), not a "primitive".
//! It is valid as a parameter, a local (a 1-slot reference, tracked in frames), and a closure result, and
//! compares with `null`. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn nullable_unit_param_and_local() {
    const SRC: &str = "fun isNull(x: Unit?) = x == null\n\
fun box(): String {\n\
    if (!isNull(null)) return \"fail 1\"\n\
    val x: Unit? = null\n\
    if (!isNull(x)) return \"fail 2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("Unit? param + local"), "OK");
}

#[test]
fn nullable_unit_closure_result() {
    const SRC: &str = "fun isNull(x: Unit?) = x == null\n\
fun box(): String {\n\
    val closure: () -> Unit? = { null }\n\
    return if (isNull(closure())) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("Unit? closure result"), "OK");
}

#[test]
fn elvis_with_null_keeps_nullable_unit_value() {
    const SRC: &str = "fun isNull(x: Unit?) = x == null\n\
fun box(): String {\n\
    val x: Unit? = null\n\
    return if (isNull(x ?: null)) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("Unit? elvis null result"), "OK");
}

#[test]
fn valueless_labeled_return_in_nullable_unit_lambda_returns_unit_value() {
    const SRC: &str = "class Inv<T>\n\
fun Inv<*>.nullableUnit(): Unit? = null\n\
fun <R> runBlock(block: () -> R): R = block()\n\
fun test(c: Inv<*>) {\n\
    runBlock {\n\
        if (true) return@runBlock\n\
        c.nullableUnit()\n\
    }\n\
}\n\
fun box(): String { test(Inv<Int>()); return \"OK\" }\n";
    assert_eq!(run(SRC).expect("valueless return in Unit? lambda"), "OK");
}
