//! An elvis `?:` at the top of a `for`-loop iterable — `for (v in listOrNull() ?: default)`. The
//! iterable start is parsed at additive precedence (to leave range operators like `..` visible), which
//! stops before the looser elvis operator, so the parser used to reject the `?:` with "expected ')'".
//! The plain-iterable branch now folds a trailing elvis chain into the iterable. Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn for_iterable_elvis_default() {
    const SRC: &str =
        "fun listOrNull(b: Boolean): List<String>? = if (b) listOf(\"x\", \"y\") else null\n\
        fun box(): String {\n\
        \x20 var n = 0\n\
        \x20 for (v in listOrNull(false) ?: listOf(\"a\")) { n += v.length }\n\
        \x20 for (v in listOrNull(true) ?: listOf(\"a\")) { n += v.length }\n\
        \x20 return if (n == 3) \"OK\" else \"fail:\" + n\n\
        }\n";
    assert_eq!(run(SRC).expect("for-iterable elvis default"), "OK");
}

#[test]
fn for_iterable_elvis_chain() {
    // A right-associative elvis chain as the iterable.
    const SRC: &str = "fun n1(): List<Int>? = null\n\
        fun n2(): List<Int>? = null\n\
        fun box(): String {\n\
        \x20 var s = 0\n\
        \x20 for (v in n1() ?: n2() ?: listOf(1, 2, 3)) { s += v }\n\
        \x20 return if (s == 6) \"OK\" else \"fail:\" + s\n\
        }\n";
    assert_eq!(run(SRC).expect("for-iterable elvis chain"), "OK");
}
