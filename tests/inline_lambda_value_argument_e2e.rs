//! A literal lambda passed to an inline function's parameter that is not declared as a function
//! type is an ordinary value, even where the call's type arguments make that parameter a function
//! type: kotlinc inlines only a lambda for an inline parameter. The body receives the lambda's
//! function object, whose method stays live. The inline function is this repository's own,
//! compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <T, R> myWith(receiver: T, block: T.() -> R): R = receiver.block()
"#;

const MAIN: &str = r#"
fun box(): String = myWith({ "OK" }) { this() }
"#;

#[test]
fn a_lambda_for_a_type_parameter_is_passed_as_a_value() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}

#[test]
fn a_lambda_for_a_type_parameter_is_passed_like_the_reference_compiler() {
    let difference = common::method_code_diff_against_kotlinc(
        "InlineValueArgument",
        &[("Lib.kt", LIB)],
        MAIN,
        "InlineValueArgumentKt",
        "public static final java.lang.String box()",
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(difference, Ok(()));
}
