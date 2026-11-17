//! A function value reaches a function-typed parameter when the parameter's result accepts the
//! value's result and the value's parameters accept the parameter's: parameters are
//! contravariant. `(Any) -> B` therefore fills `(T) -> T` with `T` inferred to `I & J`,
//! `(Any) -> Unit` fills `(T) -> Unit` with `T` inferred to `String`, and a reference taking `A?`
//! fills `(T) -> R` for the `null` receiver of `null.let`. The program runs on every
//! backend from the shared box inventory; this driver compares its result with kotlinc.
use super::common;

const SOURCE: &str = include_str!("box_regressions/FunctionArgumentParameterVariance.kt");

#[test]
fn a_function_argument_reaches_a_parameter_through_contravariant_parameters() {
    common::expect_box_same_as_kotlinc(SOURCE, "FunctionArgumentParameterVariance");
}

/// Unit conversion applies only to the passed value's own result. A function type nested in a
/// parameter compares by subtyping alone, so `(() -> Unit) -> Unit` does not fill
/// `(() -> String) -> Unit`.
#[test]
fn a_nested_function_parameter_is_not_unit_converted() {
    let source = "\
fun consume(handler: (() -> String) -> Unit) = handler { \"value\" }

fun main() {
    val unitHandler: (() -> Unit) -> Unit = { it() }
    consume(unitHandler)
}
";
    let sources = [("test.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:5:13: argument type mismatch: actual type is '(() -> Unit) -> Unit', but '(() -> String) -> Unit' was expected."
        ]
    );
}
