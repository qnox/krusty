//! Invoking a RECEIVER-function-typed value in lexical scope with member syntax: `b.f()` (and
//! `b?.f()`) where `f: Bar.() -> R` is a local/parameter and `Bar` has no member `f`. The receiver
//! becomes the function value's folded-first argument (`Function1.invoke(b)`). Mirrors corpus
//! `classes/kt1918.kt` (`(x as? Bar)?.bar()`).

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn plain_receiver_fn_param_invoke() {
    const SRC: &str = "class Bar { val v = 41 }\n\
fun call(b: Bar, f: Bar.() -> Int): Int = b.f()\n\
fun box(): String {\n\
    val r = call(Bar()) { v + 1 }\n\
    return if (r == 42) \"OK\" else \"FAIL: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("plain receiver fn invoke"), "OK");
}

#[test]
fn safe_call_receiver_fn_param_invoke() {
    const SRC: &str = "class Bar { val v = 41 }\n\
fun call(b: Bar?, f: Bar.() -> Int): Int? = b?.f()\n\
fun box(): String {\n\
    val r = call(Bar()) { v }\n\
    if (r != 41) return \"FAIL 1: $r\"\n\
    val n = call(null) { v }\n\
    if (n != null) return \"FAIL 2: $n\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("safe-call receiver fn invoke"), "OK");
}

#[test]
fn safe_cast_then_receiver_fn_invoke() {
    // The kt1918 shape: `(x as? Bar)?.bar()` where `bar` is a receiver-lambda parameter.
    const SRC: &str = "class Bar\n\
interface Foo { fun xyzzy(x: Any?): String }\n\
fun buildFoo(bar: Bar.() -> Unit): Foo {\n\
    return object : Foo {\n\
        override fun xyzzy(x: Any?): String {\n\
            (x as? Bar)?.bar()\n\
            return \"OK\"\n\
        }\n\
    }\n\
}\n\
fun box(): String {\n\
    val foo = buildFoo({})\n\
    return foo.xyzzy(Bar())\n\
}\n";
    assert_eq!(run(SRC).expect("safe-cast receiver fn invoke"), "OK");
}

#[test]
fn receiver_fn_with_value_args() {
    const SRC: &str = "class Acc { var total = 0 }\n\
fun apply2(a: Acc, op: Acc.(Int) -> Unit): Int {\n\
    a.op(40)\n\
    a.op(2)\n\
    return a.total\n\
}\n\
fun box(): String {\n\
    val r = apply2(Acc()) { n -> total += n }\n\
    return if (r == 42) \"OK\" else \"FAIL: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("receiver fn with args"), "OK");
}

#[test]
fn ctor_receiver_lambda_binds_implicit_this() {
    // KT-606: a receiver lambda passed to a CONSTRUCTOR parameter (`config: Pipeline.() -> Unit`)
    // binds the receiver as implicit `this` — a bare member call inside dispatches on the receiver,
    // not a same-named stdlib top-level (`print`).
    const SRC: &str = "var result = \"FAIL\"\n\
interface Pipeline { fun print(any: Any) }\n\
class Impl : Pipeline { override fun print(any: Any) { result = any as String } }\n\
class Factory(val config: Pipeline.() -> Unit) {\n\
    fun run(): Pipeline { val p: Pipeline = Impl(); p.config(); return p }\n\
}\n\
fun box(): String {\n\
    Factory({ print(\"OK\") }).run()\n\
    return result\n\
}\n";
    assert_eq!(run(SRC).expect("ctor receiver lambda"), "OK");
}

#[test]
fn real_member_still_wins_over_scope_value() {
    // `Bar` HAS a member `f` — member resolution must win over the same-named scope value.
    const SRC: &str = "class Bar { fun f(): Int = 1 }\n\
fun call(b: Bar, f: Bar.() -> Int): Int = b.f()\n\
fun box(): String {\n\
    val r = call(Bar()) { 2 }\n\
    return if (r == 1) \"OK\" else \"FAIL: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("member wins"), "OK");
}
