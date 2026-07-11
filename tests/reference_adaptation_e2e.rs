//! Reference adaptation: a function reference `::foo` whose target has trailing DEFAULT parameters,
//! passed where a function type of SMALLER arity is expected. kotlinc adapts the reference by
//! synthesizing an adapter that calls `foo` with the defaults filled. Before, krusty typed `::foo`
//! only at its full arity, so the shorter expected type was a mismatch ("callable references are not
//! supported"). Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn adapt_trailing_default_argument() {
    const SRC: &str = "fun foo(x: String, y: String = \"K\"): String = x + y\n\
        fun call(f: (String) -> String, x: String): String = f(x)\n\
        fun box(): String = call(::foo, \"O\")\n";
    assert_eq!(run(SRC).expect("adapt trailing default"), "OK");
}

// Coercion to `Unit`: a reference to a value-returning function passed where `() -> Unit` /
// `(T) -> Unit` is expected — the adapter calls the target and discards its result.
#[test]
fn adapt_coercion_to_unit() {
    const SRC: &str = "var log = \"\"\n\
        fun foo(x: String): String { log += x; return x }\n\
        fun call(f: (String) -> Unit, x: String) { f(x) }\n\
        fun box(): String {\n\
        \x20 call(::foo, \"OK\")\n\
        \x20 return log\n\
        }\n";
    assert_eq!(run(SRC).expect("adapt coercion to Unit"), "OK");
}

// A trailing `vararg` is dropped: the adapter passes an empty array.
#[test]
fn adapt_trailing_empty_vararg() {
    const SRC: &str = "fun foo(x: String, vararg y: String): String =\n\
        \x20 if (y.isEmpty()) x + \"K\" else \"Fail\"\n\
        fun call(f: (String) -> String, x: String): String = f(x)\n\
        fun box(): String = call(::foo, \"O\")\n";
    assert_eq!(run(SRC).expect("adapt trailing vararg"), "OK");
}

// Discarding a WIDE (2-slot) result (Long) in the coercion adapter's statement position.
#[test]
fn adapt_coercion_wide_discard() {
    const SRC: &str = "var n = 0L\n\
        fun foo(x: Long): Long { n = x; return x }\n\
        fun call(f: (Long) -> Unit) { f(9L) }\n\
        fun box(): String { call(::foo); return if (n == 9L) \"OK\" else \"Fail\" }\n";
    assert_eq!(run(SRC).expect("wide discard"), "OK");
}

#[test]
fn adapt_coercion_primitive_discard() {
    const SRC: &str = "var n = 0\n\
        fun foo(x: Int): Boolean { n = x; return true }\n\
        fun call(f: (Int) -> Unit) { f(7) }\n\
        fun box(): String { call(::foo); return if (n == 7) \"OK\" else \"Fail\" }\n";
    assert_eq!(run(SRC).expect("primitive discard"), "OK");
}

// Base support: a plain call to a function with a trailing vararg AND a defaulted fixed parameter,
// omitting the vararg (empty). Previously rejected ("expects at least 1 arg") / not lowered.
#[test]
fn default_and_empty_vararg_call() {
    const SRC: &str =
        "fun foo(s: String = \"K\", vararg t: String): String = s + t.size.toString()\n\
        fun box(): String = if (foo() == \"K0\" && foo(\"A\") == \"A0\") \"OK\" else \"Fail\"\n";
    assert_eq!(run(SRC).expect("default + empty vararg"), "OK");
}

// Combined: drop a trailing default AND a trailing vararg, coercing to Unit. Now supported because the
// base $default stub for a vararg function is emitted.
#[test]
fn adapt_default_and_vararg_to_unit() {
    const SRC: &str = "var log = \"\"\n\
        fun foo(s: String = \"K\", vararg t: String): Boolean {\n\
        \x20 log += s; log += t.size.toString(); return true\n\
        }\n\
        fun bar(f: () -> Unit) { f() }\n\
        fun box(): String { bar(::foo); return if (log == \"K0\") \"OK\" else \"Fail: $log\" }\n";
    assert_eq!(run(SRC).expect("adapt default+vararg to Unit"), "OK");
}
