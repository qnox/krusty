//! `Int.equals(Int)` and its integral siblings on Native.
//!
//! Native's stdlib implements each as a call of the external `areEqualByValue(first, second)`,
//! whose `TypedIntrinsic` has no body: the compiler supplies the comparison. Every program here is
//! also run by kotlinc and by krusty's JVM backend, so the value Native answers is kotlinc's.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

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
    let source = "fun box(): String {\n\
         \x20   val b: Byte = 7\n\
         \x20   val s: Short = 300\n\
         \x20   val i = 70000\n\
         \x20   val l = 5000000000L\n\
         \x20   if (!b.equals(7.toByte()) || b.equals(8.toByte())) return \"fail byte\"\n\
         \x20   if (!s.equals(300.toShort()) || s.equals(301.toShort())) return \"fail short\"\n\
         \x20   if (!i.equals(70000) || i.equals(70001)) return \"fail int\"\n\
         \x20   if (!l.equals(5000000000L) || l.equals(5000000001L)) return \"fail long\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("IntegralEqualsOwnWidth", source);
}

/// A nullable operand narrowed by a safe-call chain reaches the same-width overload.
#[test]
fn a_smart_cast_operand_equals_by_value() {
    let source = "fun same(x: Int?) = x?.toByte()?.hashCode()?.equals(x)\n\
         fun box(): String {\n\
         \x20   if (same(42) != true) return \"fail present\"\n\
         \x20   if (same(null) != null) return \"fail absent\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("SmartCastOperandEquals", source);
}
