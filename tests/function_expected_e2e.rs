//! Invoking something that is not a function. When the callee NAME resolves — to a local, a
//! parameter, or a property — the name is not unresolved, and kotlinc says so: FUNCTION_EXPECTED
//! reports the expression and its type, and points at the missing `invoke()`. Only a callee that
//! names nothing at all is an unresolved reference.

use super::common;

fn assert_reports(source: &str, expected: &str) {
    let Some(diagnostics) = common::checker_diags_with_stdlib(source) else {
        return;
    };
    assert!(
        diagnostics.iter().any(|diagnostic| diagnostic == expected),
        "expected {expected:?}, got: {diagnostics:?}"
    );
}

#[test]
fn calling_a_local_value_reports_the_value_and_its_type() {
    assert_reports(
        r#"
            fun use() {
                val count = 1
                count()
            }
        "#,
        "expression 'count' of type 'Int' cannot be invoked as a function. \
         Function 'invoke()' is not found.",
    );
}

#[test]
fn calling_a_parameter_reports_the_parameter_and_its_type() {
    assert_reports(
        r#"
            fun use(label: String) {
                label()
            }
        "#,
        "expression 'label' of type 'String' cannot be invoked as a function. \
         Function 'invoke()' is not found.",
    );
}

#[test]
fn a_local_value_shadowing_a_class_is_not_an_unresolved_reference() {
    const SOURCE: &str = r#"
        class Registry
        fun use() {
            val Registry = 1
            Registry()
        }
    "#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(
        diagnostics.iter().any(|diagnostic| diagnostic
            == "expression 'Registry' of type 'Int' cannot be invoked as a function. \
                Function 'invoke()' is not found."),
        "a value shadowing a classifier keeps the value's diagnostic, got: {diagnostics:?}"
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.starts_with("unresolved reference")),
        "the name resolved, so nothing is unresolved: {diagnostics:?}"
    );
}
