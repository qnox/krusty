//! `String.trimIndent()` / `String.trimMargin()` on a compile-time-constant string receiver is
//! folded to a string constant at lowering (kotlinc special-cases a constant receiver). Before, the
//! lowerer bailed with `call .trimIndent`. Same-file, runnable; the expected values match kotlinc.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn trim_indent_multiline() {
    // The common 8-space indent is removed; the blank first/last lines are dropped.
    const SRC: &str = "fun box(): String {\n\
        \x20 val s = \"\"\"\n\
        \x20       a\n\
        \x20       b\n\
        \x20       \"\"\".trimIndent()\n\
        \x20 return if (s == \"a\\nb\") \"OK\" else \"fail: [\" + s + \"]\"\n\
        }\n";
    assert_eq!(run(SRC).expect("trimIndent"), "OK");
}

#[test]
fn trim_margin_keeps_non_margin_line() {
    // A line WITHOUT the margin prefix is left UNCHANGED (verified against kotlinc 2.4.0):
    // `"  hello\n  |world".trimMargin()` == `"  hello\nworld"` (default prefix `|`).
    const SRC: &str = "fun box(): String {\n\
        \x20 val s = \"  hello\\n  |world\".trimMargin()\n\
        \x20 return if (s == \"  hello\\nworld\") \"OK\" else \"fail: [\" + s + \"]\"\n\
        }\n";
    assert_eq!(run(SRC).expect("trimMargin non-margin line"), "OK");
}

#[test]
fn trim_margin_default_prefix() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val s = \"\"\"\n\
        \x20       |x\n\
        \x20       |y\n\
        \x20       \"\"\".trimMargin()\n\
        \x20 return if (s == \"x\\ny\") \"OK\" else \"fail: [\" + s + \"]\"\n\
        }\n";
    assert_eq!(run(SRC).expect("trimMargin"), "OK");
}
