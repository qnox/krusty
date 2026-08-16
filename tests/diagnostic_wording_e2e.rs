//! Diagnostics krusty and kotlinc both report must be worded identically: krusty's text is the
//! Kotlin frontend's message template with its first letter lowercased (the LSP boundary
//! sentence-cases it again). These pin the templates that used to differ by a trailing period or an
//! invented phrasing.

use super::common;

fn diagnostics(source: &str) -> Option<Vec<String>> {
    common::checker_diags_with_stdlib(source)
}

fn assert_reports(source: &str, expected: &str) {
    let Some(diagnostics) = diagnostics(source) else {
        return;
    };
    assert!(
        diagnostics.iter().any(|diagnostic| diagnostic == expected),
        "expected {expected:?}, got: {diagnostics:?}"
    );
}

#[test]
fn an_inapplicable_binary_operator_names_the_operator() {
    assert_reports(
        r#"
            fun use(): Any = "text" - 1
        "#,
        "operator '-' cannot be applied to 'String' and 'Int'.",
    );
}

#[test]
fn an_inapplicable_comparison_names_the_operator() {
    assert_reports(
        r#"
            class Box
            fun use(a: Box, b: Box): Boolean = a < b
        "#,
        "operator '<' cannot be applied to 'Box' and 'Box'.",
    );
}

#[test]
fn break_outside_a_loop_is_only_allowed_inside_loops() {
    assert_reports(
        r#"
            fun use() {
                break
            }
        "#,
        "'break' and 'continue' are only allowed inside loops.",
    );
}

#[test]
fn a_second_vararg_parameter_is_prohibited() {
    assert_reports(
        r#"
            fun use(vararg first: Int, vararg second: Int) {}
        "#,
        "multiple vararg parameters are prohibited.",
    );
}

#[test]
fn an_override_of_nothing_is_reported_with_a_period() {
    assert_reports(
        r#"
            open class Base
            class Derived : Base() {
                override fun absent() {}
            }
        "#,
        "'absent' overrides nothing.",
    );
}
