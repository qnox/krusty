//! A lambda's class implements `FunctionN` of the expected function type, not of the type its
//! body happens to have.

use super::common;

fn assert_like_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn a_lambda_implements_its_expected_return_type() {
    assert_like_kotlinc(&[(
        "Expected.kt",
        r#"fun supply(): () -> List<Int> = { arrayListOf(1) }

fun box(): String = if (supply()() == listOf(1)) "OK" else "FAIL"
"#,
    )]);
}

#[test]
fn a_lambda_with_a_type_parameter_body_implements_its_expected_return_type() {
    assert_like_kotlinc(&[(
        "Generic.kt",
        r#"fun <T> supply(items: ArrayList<T>): () -> List<T> = { items }

fun box(): String = if (supply(arrayListOf(1))() == listOf(1)) "OK" else "FAIL"
"#,
    )]);
}
