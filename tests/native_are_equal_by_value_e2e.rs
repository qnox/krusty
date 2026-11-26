//! `Int.equals(Int)` and its integral siblings on Native.
//!
//! Native's stdlib implements each as a call of the external `areEqualByValue(first, second)`,
//! whose `TypedIntrinsic` has no body: the compiler supplies the comparison. Every program here is
//! also run by kotlinc and by krusty's JVM backend, so the value Native answers is kotlinc's.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

const OWN_WIDTH: &str = include_str!("box_regressions/integralEqualsOwnWidth.kt");
const SMART_CAST: &str = include_str!("box_regressions/smartCastOperandEquals.kt");

fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// A value equals itself and differs from another, at every integral width, with both operands
/// statically of that width.
#[test]
fn an_integral_value_equals_an_operand_of_its_own_width_by_value() {
    every_backend_agrees_with_kotlinc("IntegralEqualsOwnWidth", OWN_WIDTH);
}

/// A nullable operand narrowed by a safe-call chain reaches the same-width overload.
#[test]
fn a_smart_cast_operand_equals_by_value() {
    every_backend_agrees_with_kotlinc("SmartCastOperandEquals", SMART_CAST);
}
