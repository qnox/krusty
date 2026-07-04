//! No-receiver `run { … }` (stdlib `inline fun <R> run(block: () -> R): R`): the lambda body is inlined
//! directly as the value, including a branchy body (`run { if … }` / `run { when … }`). Round-tripped.

use super::common;

fn run_box(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn no_receiver_run_with_branchy_body() {
    const SRC: &str = "fun box(): String {\n\
    val a = run { if (1 == 1) \"O\" else \"X\" }\n\
    val b = run { val n = 2; when (n) { 2 -> \"K\"; else -> \"X\" } }\n\
    return run { a + b }\n\
}\n";
    let out = run_box(SRC).expect("no-receiver run with a branchy body should compile + run");
    assert_eq!(out, "OK");
}
