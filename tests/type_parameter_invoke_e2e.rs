//! Calling a value whose type is a type parameter bounded by a function type.
//!
//! `t(x)` on `t: T` with `T : (Int) -> Int` is the ordinary invoke convention over the bound: the
//! value claims the call exactly as a `(Int) -> Int` value would, and kotlinc calls the erased
//! `Function1.invoke` on it.

use super::common;

/// A bound written in the type-parameter list and one written in a `where` clause are the same
/// bound; both calls box the argument and unbox the erased result through `Number`.
#[test]
fn a_function_bounded_type_parameter_value_is_invoked_like_kotlinc() {
    const SRC: &str = "fun <T : (Int) -> Int> listed(t: T): Int = t(5)\n\
fun <T> constrained(t: T): Int where T : (Int) -> Int = t(6)\n";
    common::byte_diff_against_kotlinc("FunctionBoundInvoke", SRC, "FunctionBoundInvokeKt")
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn a_function_bounded_type_parameter_value_runs() {
    const SRC: &str = "fun <T : (Int) -> Int> listed(t: T): Int = t(5)\n\
fun <T> constrained(t: T): Int where T : (Int) -> Int = t(6)\n\
fun box(): String {\n\
    val double = { x: Int -> x * 2 }\n\
    return if (listed(double) == 10 && constrained(double) == 12) \"OK\" else \"fail\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "FunctionBoundInvokeRun");
}
