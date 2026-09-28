//! Top-level functions with NON-CONST (side-effecting) default arguments, called with the default
//! omitted, route through kotlinc's `foo$default(params…, int mask, Object marker)` synthetic. The
//! provided arguments are evaluated at the call site (in source order); the stub fills the masked slots
//! from the defaults.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn non_const_default_omitted() {
    // `b`'s default is a non-const expression (`compute()`); omitting `b` must run the default.
    const SRC: &str = "var log = \"\"\n\
fun compute(): String { log += \"d\"; return \"D\" }\n\
fun f(a: String, b: String = compute()): String = a + b\n\
fun box(): String {\n\
    val r = f(\"A\")\n\
    return if (r == \"AD\" && log == \"d\") \"OK\" else \"FAIL: r=$r log=$log\"\n\
}\n";
    assert_eq!(run(SRC).expect("non-const default omitted"), "OK");
}

#[test]
fn non_const_default_provided_not_run() {
    // When the defaulted argument IS provided, the default expression must NOT run.
    const SRC: &str = "var log = \"\"\n\
fun compute(): String { log += \"d\"; return \"D\" }\n\
fun f(a: String, b: String = compute()): String = a + b\n\
fun box(): String {\n\
    val r = f(\"A\", \"B\")\n\
    return if (r == \"AB\" && log == \"\") \"OK\" else \"FAIL: r=$r log=$log\"\n\
}\n";
    assert_eq!(run(SRC).expect("provided default not run"), "OK");
}

#[test]
fn default_references_earlier_parameter() {
    // A default that reads an EARLIER parameter (`z = x + y + 1`) — evaluated inside `$default` where the
    // parameters are in scope. Mirrors `function/defaults1.kt`.
    const SRC: &str = "fun foo(x: Int = 0, y: Int = x + 1, z: Int = x + y + 1) = x + y + z\n\
fun box(): String {\n\
    val v = foo()\n\
    return if (v == 3) \"OK\" else \"FAIL: $v\"\n\
}\n";
    assert_eq!(run(SRC).expect("param-referencing default"), "OK");
}

#[test]
fn int_non_const_default_omitted() {
    // A primitive (Int) non-const default omitted — the omitted-slot placeholder must be `0`, not null.
    const SRC: &str = "fun mk(): Int = 7\n\
fun f(a: Int, b: Int = mk()): Int = a + b\n\
fun box(): String {\n\
    val v = f(5)\n\
    return if (v == 12) \"OK\" else \"FAIL: $v\"\n\
}\n";
    assert_eq!(run(SRC).expect("int non-const default"), "OK");
}

#[test]
fn invoked_lambda_default_runs_after_provided_arguments() {
    // Provided named arguments evaluate at the call site, in source order. Each omitted
    // parameter's immediately-invoked lambda then runs inside `test$default`, in parameter order.
    // Mirrors `argumentOrder/defaults.kt`.
    const SRC: &str = "var invokeOrder: String = \"\"\n\
fun test(x: Double = { invokeOrder += \"x\"; 1.0 }(), a: String, y: Long = { invokeOrder += \"y\"; 1L }(), b: String): String {\n\
    return \"\" + x.toInt() + a + b + y\n\
}\n\
fun box(): String {\n\
    val funResult = test(b = { invokeOrder += \"K\"; \"K\" }(), a = { invokeOrder += \"O\"; \"O\" }())\n\
    if (invokeOrder != \"KOxy\" || funResult != \"1OK1\") return \"fail: $invokeOrder != KOxy or $funResult != 1OK1\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("invoked lambda defaults"), "OK");
}

#[test]
fn two_non_const_defaults_both_omitted() {
    // Two defaulted params, both non-const, both omitted — the `$default` synthetic fills both.
    const SRC: &str = "fun mk(s: String): String = s + s\n\
fun f(a: String, b: String = mk(\"p\"), c: String = mk(\"q\")): String = a + b + c\n\
fun box(): String {\n\
    val r = f(\"A\")\n\
    return if (r == \"Appqq\") \"OK\" else \"FAIL: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("two non-const defaults"), "OK");
}
