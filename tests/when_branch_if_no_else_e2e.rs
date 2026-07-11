//! A `when` branch body that is an `if` WITHOUT its own else must not swallow the `when`'s `else`
//! entry: `when (x) { is T -> if (c) a; else -> b }`. The `else ->` is the when's else, not the if's.
//! Runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn when_branch_if_without_else_then_when_else() {
    const SRC: &str = "fun f(x: Any): Int {\n\
        \x20 when (x) {\n\
        \x20   is Int -> if (x > 0) return 1\n\
        \x20   else -> return 2\n\
        \x20 }\n\
        \x20 return 0\n\
        }\n\
        fun box(): String =\n\
        \x20 if (f(5) == 1 && f(-1) == 0 && f(\"a\") == 2) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("when branch if no else"), "OK");
}
