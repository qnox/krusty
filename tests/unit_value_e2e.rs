//! `Unit` used as a first-class value: a `Unit?`-returning expression-body, `Unit` as an `==`/`!=`
//! operand (materializes `Unit.INSTANCE`), and a `Unit`-typed data-class component (its field/ctor-param/
//! getter use the `kotlin/Unit` reference, not the illegal `V` descriptor). Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn unit_as_equality_operand() {
    const SRC: &str = "fun bar() {}\n\
fun foo(): Any? = bar()\n\
fun box(): String {\n\
    if (foo() != bar()) return \"fail 1\"\n\
    if (bar() != Unit) return \"fail 2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("Unit as == operand"), "OK");
}

#[test]
fn nullable_unit_expression_body() {
    const SRC: &str = "fun bar() {}\n\
fun quux(): Unit? = bar()\n\
fun box(): String = if (quux() == Unit) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("Unit? expression body"), "OK");
}

#[test]
fn safe_assignment_returns_unit_when_the_receiver_is_null() {
    const SRC: &str = "fun foo(f: () -> Unit, returnIfOk: String): String {\n\
    val string = f().toString()\n\
    return if (string == \"kotlin.Unit\") returnIfOk else \"FAIL: $string;\"\n\
}\n\
class Wrapper(var s: String)\n\
fun box(): String {\n\
    val w: Wrapper? = Wrapper(\"Test\")\n\
    val lambda = { w?.s = \"X\" }\n\
    val w2: Wrapper? = null\n\
    val lambda2 = { w2?.s = \"X\" }\n\
    return foo(lambda, \"O\") + foo(lambda2, \"K\")\n\
}\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "safe assignment is Unit"),
        "OK"
    );
}

#[test]
fn unit_data_class_component() {
    const SRC: &str = "data class Holder(val u: Unit)\n\
fun box(): String {\n\
    val h = Holder(Unit)\n\
    return if (h.u == Unit) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("Unit data-class component"), "OK");
}
