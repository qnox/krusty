//! An `if (…)` / `when (…)` condition (or `when` subject-variable binding) may start and end on its
//! own line: `if(\n  a && b\n)`, `when(\n  val v = e\n) { … }`. The parser now skips newlines just
//! inside the parentheses, so the condition no longer fails with `expected an expression`. Runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn if_condition_on_fresh_lines() {
    const SRC: &str = "fun box(): String = if(\n\
        \x20 1 + 1 == 2\n\
        \x20 && 2 + 2 == 4\n\
        ) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("if condition across lines"), "OK");
}

#[test]
fn when_subject_on_fresh_line() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val r = when (\n\
        \x20   val t = 1 + 1\n\
        \x20 ) {\n\
        \x20   2 -> \"OK\"\n\
        \x20   else -> \"fail\"\n\
        \x20 }\n\
        \x20 return r\n\
        }\n";
    assert_eq!(run(SRC).expect("when subject across lines"), "OK");
}

#[test]
fn when_subject_expr_on_fresh_line() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val n = 3\n\
        \x20 return when (\n\
        \x20   n\n\
        \x20 ) {\n\
        \x20   3 -> \"OK\"\n\
        \x20   else -> \"fail\"\n\
        \x20 }\n\
        }\n";
    assert_eq!(run(SRC).expect("when subject expr across lines"), "OK");
}
