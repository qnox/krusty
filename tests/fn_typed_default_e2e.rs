//! Function-typed (lambda) default arguments, called with the default omitted, route through the
//! `foo$default(params…, int mask, Object marker)` synthetic like any non-const default: the stub
//! materializes the default lambda object into the masked slot.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn lambda_default_omitted() {
    const SRC: &str = "fun foo(f: (Int) -> Int = { it + 1 }): Int = f(41)\n\
fun box(): String {\n\
    val v = foo()\n\
    return if (v == 42) \"OK\" else \"FAIL: $v\"\n\
}\n";
    assert_eq!(run(SRC).expect("lambda default omitted"), "OK");
}

#[test]
fn lambda_default_provided_not_used() {
    const SRC: &str = "fun foo(f: (Int) -> Int = { it + 1 }): Int = f(41)\n\
fun box(): String {\n\
    val v = foo { it * 2 }\n\
    return if (v == 82) \"OK\" else \"FAIL: $v\"\n\
}\n";
    assert_eq!(run(SRC).expect("lambda default provided"), "OK");
}

#[test]
fn unit_lambda_default_omitted() {
    // `(Int) -> Unit` default that side-effects — the omitted call must run the default body.
    const SRC: &str = "var log = \"\"\n\
fun each(n: Int, f: (Int) -> Unit = { log += it }) { for (i in 0 until n) f(i) }\n\
fun box(): String {\n\
    each(3)\n\
    return if (log == \"012\") \"OK\" else \"FAIL: $log\"\n\
}\n";
    assert_eq!(run(SRC).expect("unit lambda default omitted"), "OK");
}

#[test]
fn lambda_default_captures_earlier_parameter() {
    // The default lambda reads an EARLIER parameter — built inside `$default` where `base` is in scope.
    const SRC: &str = "fun foo(base: Int, f: () -> Int = { base * 10 }): Int = f()\n\
fun box(): String {\n\
    val v = foo(4)\n\
    return if (v == 40) \"OK\" else \"FAIL: $v\"\n\
}\n";
    assert_eq!(run(SRC).expect("lambda default capturing param"), "OK");
}

#[test]
fn member_lambda_default_omitted() {
    const SRC: &str = "class C(val k: Int) {\n\
    fun get(f: (Int) -> Int = { it + k }): Int = f(1)\n\
}\n\
fun box(): String {\n\
    val v = C(10).get()\n\
    return if (v == 11) \"OK\" else \"FAIL: $v\"\n\
}\n";
    assert_eq!(run(SRC).expect("member lambda default omitted"), "OK");
}
