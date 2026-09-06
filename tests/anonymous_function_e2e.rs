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
fn anon_fun_local_return_targets_its_own_declared_type() {
    // The bare `return` binds to the ANONYMOUS FUNCTION, not the enclosing `box(): String` — so an
    // `Int`-returning anonymous function inside a `String`-returning one is legal. (The existing
    // block-body test only passes because both happen to return `String`.)
    const SRC: &str = "fun box(): String {\n\
    val f = fun(x: Int): Int { return x + 1 }\n\
    if (f(9) != 10) return \"f1\"\n\
    val g = fun(n: Int): Boolean { return n % 2 == 0 }\n\
    if (!g(4)) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("anon fun local return"), "OK");
}

#[test]
fn anon_fun_return_type_inferred_from_the_expected_function_type() {
    // The parameter type comes from the expected `(Int) -> Int`; the `return` still targets the
    // anonymous function's own declared `Int`.
    const SRC: &str = "fun box(): String {\n\
    val f: (Int) -> Int = fun(x): Int { return x + 1 }\n\
    if (f(9) != 10) return \"f1\"\n\
    val xs = listOf(1, 2, 3, 4).filter(fun(n: Int): Boolean { return n % 2 == 0 })\n\
    if (xs != listOf(2, 4)) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("anon fun under an expected function type"),
        "OK"
    );
}

#[test]
fn block_bodied_anon_fun_without_a_declared_type_returns_unit() {
    // An undeclared block body returns `Unit`, so a bare `return` is legal there and does NOT bind
    // to the enclosing function.
    const SRC: &str = "fun box(): String {\n\
    var seen = 0\n\
    val f = fun(x: Int) { if (x < 0) return; seen += x }\n\
    f(-1)\n\
    f(5)\n\
    if (seen != 5) return \"f1\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("undeclared block-bodied anon fun"), "OK");
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

// An anonymous EXTENSION function stored in a top-level property. The bare `return` inside it is
// local to the anonymous function, so checked FIR must resolve it against the anonymous function's
// own control target rather than the enclosing declaration's — the enclosing declaration here is a
// property initializer, which has no function return at all
// (`extensionFunctions/extensionFunctionAsAnonymous.kt`).
#[test]
fn anon_extension_fun_in_a_property_initializer_returns_locally() {
    const SRC: &str = "val a = fun String.(y: String): String { return this + y }\n\
        fun box(): String = if (a(\"O\", \"K\") == \"OK\") \"OK\" else \"Fail\"\n";
    assert_eq!(run(SRC).expect("anon extension fun local return"), "OK");
}

// The same local return in a plain (non-extension) anonymous function initializing a top-level
// property, so the fix cannot be specific to the extension-receiver shape.
#[test]
fn anon_fun_in_a_property_initializer_returns_locally() {
    const SRC: &str = "val a = fun (y: String): String { return y }\n\
        fun box(): String = if (a(\"OK\") == \"OK\") \"OK\" else \"Fail\"\n";
    assert_eq!(run(SRC).expect("anon fun local return"), "OK");
}
