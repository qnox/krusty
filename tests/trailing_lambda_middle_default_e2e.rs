//! A trailing lambda binding the LAST parameter of a user top-level fn when a MIDDLE default is
//! omitted — including a FUNCTION-TYPED middle default (`chk: ((Int) -> Unit)? = null`), which used
//! to capture the lambda's pre-typing positionally (typed with `chk`'s param shape → the checker
//! reported "Function but Function was expected" against `action`). Corpus:
//! coroutines/varSpilling/fakeInlinerVariables.kt's `expectFailure<Throwable>(msg) { … }`.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Tm")
}

#[test]
fn trailing_lambda_skips_fn_typed_middle_default() {
    const SRC: &str = "fun ef(msg: String? = null, chk: ((Int) -> Unit)? = null, action: () -> Unit) { action() }\n\
fun box(): String {\n\
    var r = \"FAIL\"\n\
    ef(\"m\") { r = \"OK\" }\n\
    return r\n\
}\n";
    let out =
        run(SRC).expect("trailing lambda over a fn-typed middle default should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn named_lambda_arg_skips_fn_typed_middle_default() {
    const SRC: &str = "fun ef(msg: String? = null, chk: ((Int) -> Unit)? = null, action: () -> Unit) { action() }\n\
fun box(): String {\n\
    var r = \"FAIL\"\n\
    ef(\"m\", action = { r = \"OK\" })\n\
    return r\n\
}\n";
    let out =
        run(SRC).expect("named lambda arg over a fn-typed middle default should compile + run");
    assert_eq!(out, "OK");
}

// A NON-nullable fn-typed middle default (`chk: (Int) -> Unit = {}`) now passes the CHECKER with the
// same mapping, but the omitted-default LOWERING can't fill a lambda default yet (`lower: call ef`)
// — that's a separate gap; no test until it lands (this file's cases stay runnable end-to-end).

#[test]
fn inline_fn_trailing_lambda_with_omitted_default() {
    // The INLINE expansion's arg slotting: a syntactic trailing lambda binds the LAST parameter;
    // the omitted `x` takes its default expression (substituted directly — no $default method).
    const SRC: &str = "inline fun g(x: Int = 5, action: () -> Int): Int = x + action()\n\
fun box(): String = if (g { 37 } == 42) \"OK\" else \"no\"\n";
    let out = run(SRC).expect("inline trailing lambda with omitted default should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn inline_fn_trailing_lambda_with_omitted_fn_typed_default() {
    // The fakeInlinerVariables.kt shape: an inline fn with an omitted fn-typed middle default.
    const SRC: &str = "inline fun g(x: Int = 5, noinline chk: ((Int) -> Unit)? = null, action: () -> Int): Int = x + action()\n\
fun box(): String = if (g { 37 } == 42) \"OK\" else \"no\"\n";
    let out =
        run(SRC).expect("inline trailing lambda with omitted fn default should compile + run");
    assert_eq!(out, "OK");
}
