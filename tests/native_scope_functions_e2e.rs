//! The stdlib scope functions `let`, `run`, `with`, `also` and `apply` on Native.
//!
//! Each is an inline function whose KLIB body only invokes its lambda and returns either that
//! result or its receiver. The provider publishes that body as an inline plan, so the lambda runs
//! in the caller's frame: it may return from `box` and change the caller's locals.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

#[test]
fn scope_functions_return_their_lambda_result_or_receiver() {
    let source = r#"
fun box(): String {
    if (20.let { it + 1 } != 21) return "let"
    if (run { 3 } != 3) return "run"
    if (4.run { this * 2 } != 8) return "receiver run"
    if (with(5) { this - 1 } != 4) return "with"
    if (6.also { if (it != 6) return "also value" } != 6) return "also"
    if (7.apply { if (this != 7) return "apply value" } != 7) return "apply"
    return "OK"
}
"#;
    expect_box_ok_with_stdlib(source, "NativeScopeFunctions");
    expect_native_box(source, "NativeScopeFunctions", "OK");
}

#[test]
fn scope_function_lambdas_change_the_callers_locals() {
    let source = r#"
fun box(): String {
    var seen = 0
    1.let { seen += it }
    2.also { seen += it }
    3.apply { seen += this }
    with(4) { seen += this }
    return if (seen == 10) "OK" else "seen $seen"
}
"#;
    expect_box_ok_with_stdlib(source, "NativeScopeFunctionCaptures");
    expect_native_box(source, "NativeScopeFunctionCaptures", "OK");
}
