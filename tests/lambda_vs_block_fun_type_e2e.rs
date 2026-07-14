//! A `{ … }` in branch/trailing-lambda position that CONTAINS a statement with a nested function
//! TYPE (`val u: (value: Int) -> Unit = …`) must parse as a BLOCK, not a lambda: the `->` belongs
//! to the function type, not a lambda parameter arrow. The lambda-detection scan stops at a
//! `val`/`var`/`=`/statement keyword before any depth-0 arrow. Same-file, runs on the JVM.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn named_param_fun_type_local_in_loop() {
    // The reported shape: a local `val` with a named-parameter function type, inside a loop body.
    const SRC: &str = "fun foo(x: Int) {}\n\
        fun loop(times: Int) {\n\
        \x20 var left = times\n\
        \x20 while (left > 0) {\n\
        \x20   val u: (value: Int) -> Unit = { foo(it) }\n\
        \x20   u(left--)\n\
        \x20 }\n\
        }\n\
        fun box(): String {\n\
        \x20 loop(5)\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC).expect("named-param fun type in loop"), "OK");
}

#[test]
fn fun_type_local_in_trailing_lambda() {
    // A trailing-lambda body whose first statement declares a function-typed local.
    const SRC: &str = "fun foo(x: Int) {}\n\
        fun box(): String {\n\
        \x20 run {\n\
        \x20   val u: (value: Int) -> Unit = { foo(it) }\n\
        \x20   u(1)\n\
        \x20 }\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC).expect("fun type in trailing lambda"), "OK");
}

#[test]
fn real_lambdas_still_detected() {
    // Regression guard: genuine lambdas (plain param, destructuring param) still parse as lambdas.
    const SRC: &str = "fun box(): String {\n\
        \x20 val g: (Int) -> Int = { x -> x + 1 }\n\
        \x20 val d = listOf(1 to 2).map { (a, b) -> a + b }\n\
        \x20 return if (g(1) == 2 && d[0] == 3) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("real lambdas"), "OK");
}
