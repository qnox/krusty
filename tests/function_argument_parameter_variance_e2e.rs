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
