//! Casts involving `Unit` — which is the reference type `kotlin/Unit` at the JVM. A `Unit`-returning
//! expression used as a cast operand yields the `Unit.INSTANCE` singleton; `Unit` as a cast target is
//! `kotlin/Unit`. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn unit_value_as_any() {
    const SRC: &str = "fun p(s: String) {}\n\
fun box(): String {\n\
    val x = p(\"hi\") as Any\n\
    return if (x == Unit) \"OK\" else \"fail: $x\"\n\
}\n";
    assert_eq!(run(SRC).expect("unit as Any"), "OK");
}

#[test]
fn unit_returning_call_as_unit() {
    const SRC: &str = "fun foo() {}\n\
fun box(): String {\n\
    foo() as Unit\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("foo() as Unit"), "OK");
}

#[test]
fn unit_safe_cast_to_primitive_is_null() {
    const SRC: &str = "fun foo() {}\n\
fun bar(): Int? = foo() as? Int\n\
fun box(): String = if (bar() == null) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("foo() as? Int is null"), "OK");
}

#[test]
fn primitive_safe_cast_to_unit_is_null() {
    const SRC: &str = "fun box(): String = if (4 as? Unit != null) \"fail\" else \"OK\"\n";
    assert_eq!(run(SRC).expect("4 as? Unit is null"), "OK");
}
