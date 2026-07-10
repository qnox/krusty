//! Anonymous function expressions: `fun (params): T = expr` / `fun (params): T { … }`. Unlike a
//! lambda, an anonymous function carries explicit parameter types and an explicit return type, and a
//! bare `return` inside it is LOCAL (returns from the anonymous function, not the enclosing one). It
//! desugars to the same function value a lambda produces. Before, `fun` in expression position hit
//! `expected an expression`. Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn anon_fun_expression_body() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val f = fun(x: Int): Int = x + 1\n\
        \x20 return if (f(2) == 3) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("anon fun expression body"), "OK");
}

#[test]
fn anon_fun_block_body_with_local_return() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val g = fun(s: String): String {\n\
        \x20   var ok = \"O\"\n\
        \x20   ok += s\n\
        \x20   return ok\n\
        \x20 }\n\
        \x20 return g(\"K\")\n\
        }\n";
    assert_eq!(run(SRC).expect("anon fun block body"), "OK");
}

#[test]
fn anon_fun_passed_as_argument() {
    // `fun(x: Int) = x - 1` passed where a `(Int) -> Int` is expected (invoke.kt fail 8).
    const SRC: &str = "fun apply1(p: (Int) -> Int, i: Int) = p(i)\n\
        fun box(): String =\n\
        \x20 if (apply1(fun(x: Int) = x - 1, 1) == 0) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("anon fun as argument"), "OK");
}

#[test]
fn anon_fun_immediately_invoked() {
    // `(fun (s: String): String { … }).invoke("K")` (simpleAnonymousFun.kt).
    const SRC: &str = "fun box(): String =\n\
        \x20 (fun (s: String): String {\n\
        \x20   var ok = \"O\"\n\
        \x20   ok += s\n\
        \x20   return ok\n\
        \x20 }).invoke(\"K\")\n";
    assert_eq!(run(SRC).expect("anon fun immediately invoked"), "OK");
}
