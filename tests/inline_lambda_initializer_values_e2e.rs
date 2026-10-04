//! An inline lambda inlined into a class initializer numbers its values from zero, like any other
//! body. None of those values is a constructor-property parameter of the host class: a lambda
//! parameter that shares a property parameter's number is still read from the lambda's own slot,
//! never from the property's field. The inline functions are this repository's own, compiled by
//! the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <T, R> withIt(value: T, block: (T) -> R): R = block(value)
"#;

const MAIN: &str = r#"
class Shifted(private val base: Int, private val offset: Int) {
    private val value: Int

    init {
        val shifted = withIt(offset) { it + base }
        value = shifted
    }

    fun value(): Int = value
}

fun box(): String {
    val value = Shifted(40, 2).value()
    return if (value == 42) "OK" else "FAIL $value"
}
"#;

#[test]
fn a_lambda_parameter_in_an_initializer_reads_its_own_slot() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN).expect("reference kotlinc is provisioned"),
        "OK"
    );
}

#[test]
fn an_initializer_lambda_is_inlined_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_lib("Main", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
