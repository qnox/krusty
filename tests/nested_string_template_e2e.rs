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
fn multi_dollar_templates_use_the_prefix_as_the_interpolation_threshold() {
    const SRC: &str = r#"
fun box(): String {
    val value = "OK"
    val count = 2
    val ordinary = $$"$$value$suffix"
    val raw = $$"""[$$value][$count]"""
    val expression = $$"$${value + count}"
    val extraDollar = $$$"$$$$value"
    val literal = $$$$"plain $value"
    return if (
        ordinary == "OK\$suffix" &&
        raw == "[OK][\$count]" &&
        expression == "OK2" &&
        extraDollar == "\$OK" &&
        literal == "plain \$value"
    ) "OK" else "fail"
}
"#;
    assert_eq!(run(SRC).expect("multi-dollar templates"), "OK");
}

#[test]
fn multi_dollar_templates_support_backticks_and_nested_templates() {
    const SRC: &str = r#"
fun box(): String {
    val `when` = "OK"
    val direct = $$"$$`when`"
    val nested = $$"outer $${$$"$$`when`$tail"}"
    return if (direct == "OK" && nested == "outer OK\$tail") "OK" else "fail"
}
"#;
    assert_eq!(run(SRC).expect("nested multi-dollar templates"), "OK");
}
