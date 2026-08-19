//! A constructor type parameter bound by a lambda's RETURN type.
//!
//! `class N<T>(val build: (String) -> T)` with `N { it + "K" }` is `N<String>`: nothing else in the
//! call mentions `T`, so the lambda's body is what determines it. The shaping step only substituted
//! the parameter when some OTHER argument had already bound something, and otherwise handed the
//! lambda the declared parameter with `T` erased to its bound — `(String) -> Any`. The typed-lambda
//! path then imposed that on the body, so the recorded return was `Any` and the call had nothing to
//! bind `T` from.
//!
//! An unbound result variable is an inference OUTPUT, not an expected type; keeping it unbound is
//! what lets the body decide it.
use super::common;

#[test]
fn a_lambda_return_binds_the_type_parameter_in_a_declaration() {
    const SRC: &str = "class N<T>(val build: (String) -> T)\n\
        val n = N { it + \"K\" }\n\
        fun box(): String = n.build(\"O\")\n";
    common::expect_box_ok_with_stdlib(SRC, "CtorLambdaReturnDeclaration");
}

#[test]
fn a_lambda_return_binds_the_type_parameter_in_a_function_body() {
    // The same call in a body, which never worked: both positions go through the one shaping step.
    const SRC: &str = "class N<T>(val build: (String) -> T)\n\
        fun box(): String { val n = N { it + \"K\" }; return n.build(\"O\") }\n";
    common::expect_box_ok_with_stdlib(SRC, "CtorLambdaReturnBody");
}

#[test]
fn an_argument_that_binds_the_parameter_still_shapes_the_lambda() {
    // The control: when another argument DOES bind the variable, the lambda must still be shaped by
    // it — `seed` fixes `T`, so `it` is an `Int` and its member reads resolve.
    const SRC: &str = "class P<T>(val seed: T, val show: (T) -> String)\n\
        val p = P(41) { (it + 1).toString() }\n\
        fun box(): String = if (p.show(p.seed) == \"42\") \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "CtorLambdaBoundParam");
}
