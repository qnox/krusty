//! A nested call whose only evidence is the parameter it fills (`consume(produce())` with
//! `fun <T> produce(): T`) is applicable to that parameter in every call selection, including the
//! one that infers a declaration's result type. kotlinc completes `T` from the parameter.
use super::common;

#[test]
fn a_result_only_call_argument_selects_its_callee_in_an_inferred_result() {
    let source = include_str!("box_regressions/resultOnlyCallArgumentInInferredResult.kt");
    common::expect_box_ok_with_stdlib(source, "ResultOnlyCallArgumentInInferredResult");
}
