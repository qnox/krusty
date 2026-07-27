//! A string template may nest: a string literal inside a `${…}` interpolation may itself contain
//! `${…}` (`"1 ${"2 ${3} 5"} 6"`). The lexer expands a nested template by queueing its tokens, so the
//! enclosing `${…}` must drain that queue in order. Runnable; the interpolated result matches kotlinc.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn nested_template_two_levels() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val x = \"1 ${\"2 ${3} 5\"} 6\"\n\
        \x20 return if (x == \"1 2 3 5 6\") \"OK\" else \"fail: \" + x\n\
        }\n";
    assert_eq!(run(SRC).expect("nested template"), "OK");
}

#[test]
fn nested_template_multiple_inner() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val x = \"a ${\"b ${1} c ${2} d\"} e\"\n\
        \x20 return if (x == \"a b 1 c 2 d e\") \"OK\" else \"fail: \" + x\n\
        }\n";
    assert_eq!(run(SRC).expect("nested template multi"), "OK");
}

#[test]
fn multi_dollar_templates_preserve_literal_runs_and_raw_quotes() {
    const SRC: &str = r#"
fun box(): String {
    val value = "OK"
    val empty = $""
    val plain = $$"plain"
    val rawPlain = $$$"""raw"""
    val single = $"[$value]"
    val regular = $$"[$value][$$value][$$$value]"
    val raw = $$"""[$value][$$value]"""""
    val literal = $$$"plain $$value"
    if (empty != "" || plain != "plain" || rawPlain != "raw") return "plain"
    if (single != "[OK]") return "single: $single"
    if (regular != "[\$value][OK][\$OK]") return "regular: $regular"
    if (raw != "[\$value][OK]\"\"") return "raw: $raw"
    if (literal != "plain \$\$value") return "literal: $literal"
    return "OK"
}
"#;
    assert_eq!(run(SRC).expect("multi-dollar templates"), "OK");
}

#[test]
fn multi_dollar_templates_support_braced_backtick_and_nested_values() {
    const SRC: &str = r#"
fun box(): String {
    val `when` = "O"
    val count = 1
    val direct = $$"$$`when`$${count}"
    val nested = $$"[$${$$"$$`when`$${count}"}]"
    return if (direct == "O1" && nested == "[O1]") "OK" else "$direct|$nested"
}
"#;
    assert_eq!(run(SRC).expect("nested multi-dollar templates"), "OK");
}
