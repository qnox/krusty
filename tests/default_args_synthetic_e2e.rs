//! Top-level functions with NON-CONST (side-effecting) default arguments, called with the default
//! omitted, route through kotlinc's `foo$default(params…, int mask, Object marker)` synthetic. The
//! provided arguments are evaluated at the call site (in source order); the stub fills the masked slots
//! from the defaults.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
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
