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

#[test]
fn omitted_vararg_with_a_declared_default_uses_that_array() {
    // A vararg that declares a default is not the implicit empty pack. Omitting it sets the
    // parameter's mask bit so `$default` evaluates the array. A supplied element still packs.
    const SRC: &str = "fun pack(vararg arr: Int = intArrayOf(1, 2)): Int {\n\
    var sum = 0\n\
    for (value in arr) sum += value\n\
    return sum\n\
}\n\
fun box(): String {\n\
    val omitted = pack()\n\
    val given = pack(42)\n\
    return if (omitted == 3 && given == 42) \"OK\" else \"omitted=$omitted given=$given\"\n\
}\n";
    assert_eq!(run(SRC).expect("vararg default omitted"), "OK");
}

#[test]
fn omitted_constructor_vararg_with_a_declared_default_uses_that_array() {
    // Same rule at a constructor: `C()` masks the vararg so `<init>$default` builds
    // `intArrayOf(1, 2)`. `C(42)` packs the element and leaves the mask clear. A secondary
    // constructor that delegates with `this()` takes the same omitted default.
    const SRC: &str = "class C(vararg val xs: Int = intArrayOf(1, 2)) {\n\
    constructor(flag: Boolean) : this()\n\
    fun sum(): Int {\n\
        var total = 0\n\
        for (value in xs) total += value\n\
        return total\n\
    }\n\
}\n\
fun box(): String {\n\
    val omitted = C().sum()\n\
    val given = C(42).sum()\n\
    val delegated = C(true).sum()\n\
    return if (omitted == 3 && given == 42 && delegated == 3) \"OK\" else \"omitted=$omitted given=$given delegated=$delegated\"\n\
}\n";
    assert_eq!(run(SRC).expect("constructor vararg default omitted"), "OK");
}

#[test]
fn omitted_extension_vararg_with_a_declared_default_uses_that_array() {
    const SRC: &str = "fun String.pack(vararg arr: Int = intArrayOf(1, 2)): Int {\n\
    var sum = 0\n\
    for (value in arr) sum += value\n\
    return sum\n\
}\n\
fun box(): String {\n\
    val omitted = \"x\".pack()\n\
    val given = \"y\".pack(42)\n\
    return if (omitted == 3 && given == 42) \"OK\" else \"omitted=$omitted given=$given\"\n\
}\n";
    assert_eq!(run(SRC).expect("extension vararg default omitted"), "OK");
}

#[test]
fn omitted_member_extension_vararg_with_a_declared_default_uses_that_array() {
    const SRC: &str = "class Host {\n\
    fun String.pack(vararg arr: Int = intArrayOf(1, 2)): Int {\n\
        var sum = 0\n\
        for (value in arr) sum += value\n\
        return sum\n\
    }\n\
    fun test(): String {\n\
        val omitted = \"x\".pack()\n\
        val given = \"y\".pack(42)\n\
        return if (omitted == 3 && given == 42) \"OK\" else \"omitted=$omitted given=$given\"\n\
    }\n\
}\n\
fun box(): String = Host().test()\n";
    assert_eq!(
        run(SRC).expect("member extension vararg default omitted"),
        "OK"
    );
}
