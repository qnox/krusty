//! An `if (…)` / `while (…)` / `do … while (…)` / `when (…)` condition (or `when` subject-variable
//! binding) may start and end on its own line: `if(\n  a && b\n)`, `while (\n  true\n)`,
//! `when(\n  val v = e\n) { … }`. The parser skips newlines just inside the parentheses, so the
//! condition no longer fails with `expected an expression`.
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

const LOOP_SOURCE: &str = "package store\n\
                           \n\
                           class Counter {\n\
                           \x20   var n = 0\n\
                           \x20   fun bump() { n += 1 }\n\
                           }\n\
                           \n\
                           fun constant(c: Counter): Int {\n\
                           \x20   while (\n\
                           \x20       true\n\
                           \x20   ) {\n\
                           \x20       c.bump()\n\
                           \x20       if (c.n > 2) break\n\
                           \x20   }\n\
                           \x20   return c.n\n\
                           }\n\
                           \n\
                           fun bounded(c: Counter, limit: Int): Int {\n\
                           \x20   while (\n\
                           \x20       c.n < limit\n\
                           \x20   ) c.bump()\n\
                           \x20   do {\n\
                           \x20       c.bump()\n\
                           \x20   } while (\n\
                           \x20       c.n < limit + 2\n\
                           \x20   )\n\
                           \x20   return c.n\n\
                           }\n";

#[test]
fn loop_conditions_on_fresh_lines_are_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "LoopConditionNewline",
        LOOP_SOURCE,
        "store/LoopConditionNewlineKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/LoopConditionNewlineKt differs from kotlinc: {diff}"));
}
