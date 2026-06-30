//! `Nothing?` (a function returning the nullable bottom type) is a real nullable reference — it yields
//! `null` (it does NOT diverge), unlike `Nothing`. A `Nothing?`-returning call must not be terminated like
//! a `Nothing` call, and `expr ?: default` on a `Nothing?` lhs takes the default (the lhs is always null).
//! A branch whose type is a `Nothing`-returning call joins as the bottom type. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn nullable_nothing_elvis_takes_default() {
    const SRC: &str = "fun expectFail(f: () -> Unit): Nothing? {\n\
    try { f() } catch (e: ArithmeticException) { return null }\n\
    throw AssertionError(\"expected\")\n\
}\n\
fun box(): String {\n\
    val a = expectFail { 1 / 0 } ?: 7\n\
    return if (a == 7) \"OK\" else \"fail $a\"\n\
}\n";
    assert_eq!(run(SRC).expect("Nothing? elvis"), "OK");
}

#[test]
fn nothing_call_branch_joins_as_bottom() {
    const SRC: &str = "fun fail(): Nothing = throw RuntimeException(\"x\")\n\
fun pick(b: Boolean): Int = if (b) 5 else fail()\n\
fun box(): String = if (pick(true) == 5) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("Nothing-call branch join"), "OK");
}
