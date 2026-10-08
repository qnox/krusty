//! `0.0..2.0` as a value: a floating-point range.
//!
//! Not a progression. There is no next floating-point number for Kotlin to name, so there is no
//! walk and no step — only a pair of bounds and the question `value in it`. That is why it gets a
//! shape of its own in the runtime rather than joining the integral ranges, whose bounds are
//! 64-bit integers read at the element's signedness.
//!
//! A `Float` range is stored at `Double`: widening a float is exact and order-preserving, so every
//! comparison answers what float comparison would, and `start` narrows back to the float it was
//! built from.
//!
//! NaN needs no case of its own. `contains` is `value >= start && value <= end` and `isEmpty` is
//! `!(start <= end)`, which is Kotlin's own `lessThanOrEquals` on these types — so a NaN bound
//! makes the range empty and a NaN value belongs to nothing.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// The bounds, membership and emptiness, at both widths.
#[test]
fn a_floating_point_range_answers_its_bounds_and_what_lies_between_them() {
    let source = "fun box(): String {\n\
         \x20   val range = 0.0..2.0\n\
         \x20   if (1.0 !in range) return \"fail in\"\n\
         \x20   if (3.0 in range) return \"fail out\"\n\
         \x20   if (0.0 !in range || 2.0 !in range) return \"fail ends\"\n\
         \x20   if (range.start != 0.0) return \"fail start\"\n\
         \x20   if (range.endInclusive != 2.0) return \"fail endInclusive\"\n\
         \x20   if (range.isEmpty()) return \"fail isEmpty\"\n\
         \x20   if (!(2.0..0.0).isEmpty()) return \"fail reversed\"\n\
         \x20   val floats = 0.0f..2.0f\n\
         \x20   if (1.0f !in floats) return \"fail float in\"\n\
         \x20   if (floats.start != 0.0f) return \"fail float start\"\n\
         \x20   if (floats.endInclusive != 2.0f) return \"fail float end\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "FloatingRange");
    expect_native_box(source, "FloatingRange", "OK");
}

/// A NaN bound makes the range empty, and a NaN value belongs to nothing.
#[test]
fn a_range_with_a_nan_bound_is_empty_and_holds_nothing() {
    let source = "fun box(): String {\n\
         \x20   val nan = Double.NaN\n\
         \x20   val range = 0.0..nan\n\
         \x20   if (1.0 in range) return \"fail in\"\n\
         \x20   if (0.0 in range) return \"fail start in\"\n\
         \x20   if (nan in range) return \"fail nan in\"\n\
         \x20   if (!range.isEmpty()) return \"fail isEmpty\"\n\
         \x20   if (nan in 0.0..2.0) return \"fail nan in an ordinary range\"\n\
         \x20   if (!(nan..nan).isEmpty()) return \"fail both bounds\"\n\
         \x20   // A range written in place answers the same way one held in a variable does.\n\
         \x20   if ((1.0 in range) != (1.0 in 0.0..nan)) return \"fail agreement\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "FloatingRangeNaN");
    expect_native_box(source, "FloatingRangeNaN", "OK");
}

/// The three `kotlin.Any` members, which Kotlin's own declaration answers by the bounds.
#[test]
fn two_floating_point_ranges_are_equal_when_their_bounds_are() {
    let source = "fun box(): String {\n\
         \x20   if (0.0..2.0 != 0.0..2.0) return \"fail equal\"\n\
         \x20   if (0.0..2.0 == 0.0..3.0) return \"fail unequal\"\n\
         \x20   if ((0.0..2.0).hashCode() != (0.0..2.0).hashCode()) return \"fail hashCode\"\n\
         \x20   // Every empty range equals every other, which is what makes a NaN bound compare\n\
         \x20   // at all: NaN is equal to no number, its own self included.\n\
         \x20   val nan = Double.NaN\n\
         \x20   if (nan..nan != 2.0..0.0) return \"fail empty\"\n\
         \x20   if (\"\" + (0.0..2.0) != \"0.0..2.0\") return \"fail toString\"\n\
         \x20   // A `Float` range is never a `Double` one, whatever its bounds.\n\
         \x20   if ((0.0f..2.0f) as Any == (0.0..2.0) as Any) return \"fail widths\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "FloatingRangeIdentity");
    expect_native_box(source, "FloatingRangeIdentity", "OK");
}

/// A range held under the plainer `ClosedRange<Double>` is the same object and answers the same.
#[test]
fn a_floating_point_range_read_through_closed_range_answers_the_same() {
    let source = "fun box(): String {\n\
         \x20   val range: ClosedRange<Double> = 0.0..2.0\n\
         \x20   if (1.0 !in range) return \"fail in\"\n\
         \x20   if (range.start != 0.0) return \"fail start\"\n\
         \x20   if (range.endInclusive != 2.0) return \"fail endInclusive\"\n\
         \x20   if (range.isEmpty()) return \"fail isEmpty\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "FloatingRangeAsClosedRange");
    expect_native_box(source, "FloatingRangeAsClosedRange", "OK");
}
