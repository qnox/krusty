//! Named arguments on a same-file USER instance method, including REORDERING with side effects. Kotlin
//! evaluates the receiver first, then the arguments in written (source) order, binding each label to its
//! parameter position.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn member_reordered_named_args_source_order() {
    // `z.test(b = …, a = …)` on a plain method: side effects run in SOURCE order (b then a → "KO"); the
    // call binds by position (a + b + p → "OKZ").
    const SRC: &str = "class Z(val p: String) {\n\
    fun test(a: String, b: String): String = a + b + p\n\
}\n\
fun box(): String {\n\
    var res = \"\"\n\
    val call = Z(\"Z\").test(b = { res += \"K\"; \"K\" }(), a = { res += \"O\"; \"O\" }())\n\
    return if (res == \"KO\" && call == \"OKZ\") \"OK\" else \"FAIL: res=$res call=$call\"\n\
}\n";
    assert_eq!(run(SRC).expect("member reordered named args"), "OK");
}

#[test]
fn member_reordered_named_args_receiver_side_effect_first() {
    // The RECEIVER's side effect runs before the arguments' (res: "1" then "K" then "O").
    const SRC: &str = "class C(val x: String) {\n\
    fun test(a: String, b: String): String = x + a + b\n\
}\n\
var res = \"\"\n\
fun mk(): C { res += \"1\"; return C(\"x\") }\n\
fun box(): String {\n\
    val call = mk().test(b = { res += \"K\"; \"K\" }(), a = { res += \"O\"; \"O\" }())\n\
    return if (res == \"1KO\" && call == \"xOK\") \"OK\" else \"FAIL: res=$res call=$call\"\n\
}\n";
    assert_eq!(run(SRC).expect("receiver-first eval"), "OK");
}

#[test]
fn extension_reordered_named_args_source_order() {
    // Same for a user EXTENSION function (`"x".test(b = …, a = …)`) — receiver first, args source order.
    const SRC: &str = "fun String.test(a: String, b: String): String = this + a + b\n\
fun box(): String {\n\
    var res = \"\"\n\
    val call = { res += \"1\"; \"x\" }().test(b = { res += \"K\"; \"K\" }(), a = { res += \"O\"; \"O\" }())\n\
    return if (res == \"1KO\" && call == \"xOK\") \"OK\" else \"FAIL: res=$res call=$call\"\n\
}\n";
    assert_eq!(run(SRC).expect("extension reordered named args"), "OK");
}

#[test]
fn member_named_args_reorder_lambda_param() {
    // A named argument bound to a `() -> String` parameter, reordered BEFORE the `String` parameters:
    // the checker must type-check each argument against its NAMED parameter (not positionally), else
    // the lambda `c` is checked against `String`. Mirrors `argumentOrder/simpleInClass.kt`.
    const SRC: &str = "class Z(val p: String) {\n\
    fun test(a: String, b: String, c: () -> String): String = a + b + c() + p\n\
}\n\
fun box(): String {\n\
    var res = \"\"\n\
    val call = Z(\"Z\").test(c = { res += \"L\"; \"L\" }, b = { res += \"K\"; \"K\" }(), a = { res += \"O\"; \"O\" }())\n\
    return if (res == \"KOL\" && call == \"OKLZ\") \"OK\" else \"FAIL: res=$res call=$call\"\n\
}\n";
    assert_eq!(run(SRC).expect("reordered lambda-param named arg"), "OK");
}

#[test]
fn member_named_args_in_order_still_work() {
    // A non-reordered named call is unaffected.
    const SRC: &str = "class Z {\n\
    fun test(a: String, b: String): String = a + b\n\
}\n\
fun box(): String = if (Z().test(a = \"O\", b = \"K\") == \"OK\") \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("in-order named args"), "OK");
}
