//! A primitive array spread passed to a local function's `vararg`. kotlinc measures the spread
//! against the whole `IntArray`, so the local function stays applicable and wins over the stdlib
//! `sum` overloads the same call could otherwise reach. The program runs on every backend from the
//! shared box inventory; this driver also compares its class bytes with kotlinc.
use super::common;

const SOURCE: &str = include_str!("box_regressions/LocalVarargPrimitiveSpread.kt");

#[test]
fn a_primitive_array_spread_fills_a_local_vararg() {
    common::assert_class_matches_kotlinc(
        "LocalVarargPrimitiveSpread",
        SOURCE,
        "LocalVarargPrimitiveSpreadKt",
    );
    common::expect_box_same_as_kotlinc(SOURCE, "LocalVarargPrimitiveSpreadRun");
}
