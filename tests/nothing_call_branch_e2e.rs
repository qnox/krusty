//! A `Nothing`-returning function CALL (not `throw`/`return`) used as a branch of an `if`/`when`
//! statement must terminate that path — kotlinc discards the physical `Void` the call leaves and throws
//! `KotlinNothingValueException`. Without that, the diverging branch leaks a `Void` into the merge frame
//! (VerifyError: inconsistent stackmap frames). Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn nothing_call_in_else_branch() {
    const SRC: &str = "var flag = true\n\
fun exit(): Nothing = throw RuntimeException(\"boom\")\n\
fun box(): String {\n\
    var a: String\n\
    if (flag) { a = \"OK\" } else { exit() }\n\
    return a\n\
}\n";
    assert_eq!(run(SRC).expect("Nothing call in else branch"), "OK");
}

#[test]
fn nothing_call_in_if_expression_value() {
    const SRC: &str = "fun fail(): Nothing = throw RuntimeException(\"x\")\n\
fun pick(b: Boolean): String {\n\
    val s = if (b) \"yes\" else fail()\n\
    return s\n\
}\n\
fun box(): String = if (pick(true) == \"yes\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("Nothing call in if-expression value"), "OK");
}
