//! A REORDERED named-argument call (`f(b = …, a = …)`) evaluates its arguments in SOURCE order
//! (Kotlin semantics), then binds them to parameter positions. Side effects must run in written order.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn reordered_named_args_evaluate_in_source_order() {
    // `test(b = …, a = …)`: side effects append in SOURCE order (b then a → "KO"); the call binds by
    // parameter position (a + b → "OK").
    const SRC: &str = "fun test(a: String, b: String): String = a + b\n\
fun box(): String {\n\
    var res = \"\"\n\
    val call = test(b = { res += \"K\"; \"K\" }(), a = { res += \"O\"; \"O\" }())\n\
    return if (res == \"KO\" && call == \"OK\") \"OK\" else \"FAIL: res=$res call=$call\"\n\
}\n";
    assert_eq!(run(SRC).expect("reordered named args"), "OK");
}

#[test]
fn reordered_named_args_three_params() {
    const SRC: &str = "fun test(a: String, b: String, c: String): String = a + b + c\n\
fun box(): String {\n\
    var res = \"\"\n\
    val call = test(c = { res += \"L\"; \"L\" }(), a = { res += \"O\"; \"O\" }(), b = { res += \"K\"; \"K\" }())\n\
    return if (res == \"LOK\" && call == \"OKL\") \"OK\" else \"FAIL: res=$res call=$call\"\n\
}\n";
    assert_eq!(run(SRC).expect("three reordered named args"), "OK");
}

#[test]
fn reordered_named_ctor_args_source_order() {
    // Same, but a constructor call (`C(b = …, a = …)`).
    const SRC: &str = "class C(val a: String, val b: String)\n\
fun box(): String {\n\
    var res = \"\"\n\
    val c = C(b = { res += \"K\"; \"K\" }(), a = { res += \"O\"; \"O\" }())\n\
    return if (res == \"KO\" && c.a + c.b == \"OK\") \"OK\" else \"FAIL: res=$res\"\n\
}\n";
    assert_eq!(run(SRC).expect("reordered ctor named args"), "OK");
}

#[test]
fn mixed_named_and_positional_args_keep_matching_slots() {
    const SRC: &str = "fun render(\n\
    first: Int = 1,\n\
    second: String = \"2\",\n\
    third: Double = 3.0,\n\
    fourth: Char = '4'\n\
): String = \"$first $second ${third.toInt()} $fourth\"\n\
fun box(): String {\n\
    val value = render(1, second = \"2\", 3.0)\n\
    return if (value == \"1 2 3 4\") \"OK\" else \"fail: $value\"\n\
}\n";
    assert_eq!(run(SRC).expect("mixed named and positional args"), "OK");
}
