//! `d === d` — IDENTITY equality between two floating-point values.
//!
//! Kotlin's `===` between primitives is its `==`, which is the whole of what the deprecation
//! warning on the form says about it. For floating-point values that carries IEEE's answers with
//! it: `NaN` is identical to nothing, itself included, and `-0.0` is identical to `0.0`. Comparing
//! the BITS would answer the opposite of both, and boxing each side and comparing addresses would
//! answer that no two values are ever identical.
//!
//! Every expectation is kotlinc's, taken by running the same program under it — including the two
//! that decide between the readings.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// A value is identical to itself, at every width the corpus case walks.
#[test]
fn a_primitive_is_identical_to_itself() {
    let source = "fun box(): String {\n\
         \x20   val i: Int = 10000\n\
         \x20   if (!(i === i)) return \"fail int ===\"\n\
         \x20   if (i !== i) return \"fail int !==\"\n\
         \x20   val j: Long = 123L\n\
         \x20   if (!(j === j)) return \"fail long ===\"\n\
         \x20   if (j !== j) return \"fail long !==\"\n\
         \x20   val d: Double = 3.14\n\
         \x20   if (!(d === d)) return \"fail double ===\"\n\
         \x20   if (d !== d) return \"fail double !==\"\n\
         \x20   val f: Float = 3.14f\n\
         \x20   if (!(f === f)) return \"fail float ===\"\n\
         \x20   if (f !== f) return \"fail float !==\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("PrimitiveIdenticalToItself", source);
}

/// Two DIFFERENT values are not identical, in an expression whose result is unused — which is the
/// shape the corpus case pins, because an unused comparison is still emitted.
#[test]
fn two_different_floating_point_values_are_not_identical() {
    let source = "fun box(): String {\n\
         \x20   (40.523 !== 62.562)\n\
         \x20   if (40.523 === 62.562) return \"fail\"\n\
         \x20   if (!(40.523 !== 62.562)) return \"fail not\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("DifferentFloatsNotIdentical", source);
}

/// `NaN` is identical to NOTHING, itself included. This is what separates the machine's comparison
/// from a bit comparison, which would answer that `NaN` is identical to itself.
#[test]
fn a_nan_is_identical_to_nothing_including_itself() {
    let source = "fun box(): String {\n\
         \x20   val nan: Double = Double.NaN\n\
         \x20   if (nan === nan) return \"fail nan === nan\"\n\
         \x20   if (!(nan !== nan)) return \"fail nan !== nan\"\n\
         \x20   val nanf: Float = Float.NaN\n\
         \x20   if (nanf === nanf) return \"fail nanf\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("NanIdenticalToNothing", source);
}

/// `-0.0` IS identical to `0.0`, which is the other half of the same distinction: their bits
/// differ, and the machine's comparison says they are equal.
#[test]
fn a_negative_zero_is_identical_to_zero() {
    let source = "fun box(): String {\n\
         \x20   val negative: Double = -0.0\n\
         \x20   val zero: Double = 0.0\n\
         \x20   if (!(negative === zero)) return \"fail ===\"\n\
         \x20   if (negative !== zero) return \"fail !==\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("NegativeZeroIdenticalToZero", source);
}
