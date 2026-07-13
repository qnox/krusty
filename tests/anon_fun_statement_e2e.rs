//! An anonymous function `fun (…) …` in STATEMENT position (e.g. a single-statement loop body,
//! `for (…) fun () {}`) is an anonymous-function EXPRESSION, not a local named declaration — the
//! statement parser must route a `fun` directly followed by `(` to the expression path. Same-file,
//! runs on the JVM. A named/generic/receiver local `fun` keeps the declaration path.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn anonymous_function_as_loop_body() {
    const SRC: &str = "fun box(): String {\n\
        \x20 for (i in 0..0) fun () {}\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC).expect("anon fun loop body"), "OK");
}

#[test]
fn named_local_fun_still_declares() {
    // Regression guard: a named local function (`fun name(...)`) stays a local declaration and is
    // callable — the anon-fun routing only diverts a `fun` directly followed by `(`.
    const SRC: &str = "fun box(): String {\n\
        \x20 fun greet(s: String): String = s\n\
        \x20 return greet(\"OK\")\n\
        }\n";
    assert_eq!(run(SRC).expect("named local fun"), "OK");
}
