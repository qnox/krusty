//! A lambda whose body type is `Nothing` — it `throw`s, or leaves only through a `return@label`
//! rather than a bare non-local `return`. Such a lambda materializes as an ordinary closure whose
//! impl method simply never falls off its end, so it needs no special modelling; krusty used to skip
//! the whole file on it, which is what made `runCatching { throw … }` uncompilable.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn throwing_lambda_passed_to_a_non_inline_function() {
    const SRC: &str = "fun take(f: () -> Int): Int = f()\n\
fun box(): String {\n\
    val r = try { take { throw RuntimeException(\"e\") } } catch (e: RuntimeException) { 7 }\n\
    return if (r == 7) \"OK\" else \"fail: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("throwing lambda as a closure"), "OK");
}

#[test]
fn throwing_lambda_makes_a_failed_result() {
    const SRC: &str = "fun box(): String {\n\
    val f: Result<Int> = runCatching { throw RuntimeException(\"e\") }\n\
    if (!f.isFailure) return \"f1\"\n\
    val ok: Result<Int> = runCatching { 3 + 4 }\n\
    if (ok.getOrThrow() != 7) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("diverging runCatching lambda"), "OK");
}

#[test]
fn labelled_return_only_lambda_runs_as_a_closure() {
    const SRC: &str = "fun take(f: () -> Int): Int = f()\n\
fun box(): String {\n\
    val r = take label@{ return@label 7 }\n\
    return if (r == 7) \"OK\" else \"fail: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("labelled-return lambda"), "OK");
}
