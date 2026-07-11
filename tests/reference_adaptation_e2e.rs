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
