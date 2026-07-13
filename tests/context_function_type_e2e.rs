//! A context-receiver function TYPE (`context(C) () -> R`, `+ContextParameters`) is modeled as a plain
//! function type with the context receivers as LEADING parameters — identical to `(C) -> R`. So a plain
//! function value converts to a context-function-typed parameter, and invoking it passes the context as
//! the first argument. Same-file, runs on the JVM.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn plain_function_converts_to_context_function_type() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        fun withContext(f: context(String) () -> String) = f(\"OK\")\n\
        fun callWithContext(f: (String) -> String) = withContext(f)\n\
        fun box(): String = callWithContext { s -> s }\n";
    assert_eq!(run(SRC).expect("context-fn-type conversion"), "OK");
}

#[test]
fn context_and_value_params_flatten() {
    // `context(A) (B) -> R` ≡ `(A, B) -> R`: the context receiver precedes the value parameters.
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        fun apply2(f: context(Int) (Int) -> Int) = f(10, 5)\n\
        fun box(): String {\n\
        \x20 val g: (Int, Int) -> Int = { a, b -> a - b }\n\
        \x20 return if (apply2(g) == 5) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("context + value params"), "OK");
}
