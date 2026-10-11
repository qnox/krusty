//! A `return` in a value class's secondary constructor body.
//!
//! The secondary constructor is a static `constructor-impl` returning the constructed carrier, so a
//! `return` in its body returns that value. An operand other than the `Unit` singleton is still
//! evaluated first, as kotlinc does.

use super::common;

const SRC: &str = "var log = \"\"\n\
    fun note(): Unit { log += \"n\" }\n\
    @JvmInline value class Foo(val x: String) {\n\
    \x20   constructor(y: Int) : this(\"OK\") {\n\
    \x20       if (y == 0) log += \"z\"\n\
    \x20       if (y == 1) return\n\
    \x20       return Unit\n\
    \x20   }\n\
    \x20   constructor(flag: Boolean) : this(\"F\") {\n\
    \x20       if (flag) return note()\n\
    \x20       log += \"t\"\n\
    \x20   }\n\
    }\n\
    fun box(): String {\n\
    \x20   if (Foo(true).x != \"F\" || log != \"n\") return \"fail: early return\"\n\
    \x20   if (Foo(false).x != \"F\" || log != \"nt\") return \"fail: fall through\"\n\
    \x20   return Foo(2).x\n\
    }\n";

fn assert_same_method(method: &str) {
    match common::class_bytes_diff_against_kotlinc("Main", &[], SRC, "Foo", method) {
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("{diff}"),
        None => panic!("Foo.{method}: reference toolchain unavailable"),
    }
}

#[test]
fn a_bare_and_a_unit_return_yield_the_constructed_value() {
    assert_same_method("public static java.lang.String constructor-impl(int)");
}

#[test]
fn a_return_operand_is_evaluated_before_the_constructed_value() {
    assert_same_method("public static java.lang.String constructor-impl(boolean)");
}

#[test]
fn secondary_constructor_returns_run() {
    let output = common::compile_and_run_box(
        SRC,
        "Main",
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
    )
    .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}
