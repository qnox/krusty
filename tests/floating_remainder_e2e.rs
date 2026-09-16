//! Exact floating-point remainder semantics across the compiler/runtime boundary.

use super::common;

#[test]
fn the_remainder_of_two_floating_point_values_is_exact() {
    // Kotlin's `%` on floating point is IEEE's remainder TRUNCATED toward zero: the sign of
    // the left operand, a magnitude below the right one's. No instruction provides it on
    // every target, so the runtime computes it on the significands — exactly, which is what
    // `1.0E16 % 3.0` and the subnormal pair are here to pin, since an implementation that
    // reached for division would lose them.
    common::expect_box_same_as_kotlinc(
        "fun frem(a: Float, b: Float): Float = a % b\n\
         fun drem(a: Double, b: Double): Double = a % b\n\
         fun box(): String {\n\
         \x20   if (drem(5.5, 2.0) != 1.5) return \"fail: 5.5 % 2.0 = \" + drem(5.5, 2.0)\n\
         \x20   if (drem(-5.5, 2.0) != -1.5) return \"fail: -5.5 % 2.0 = \" + drem(-5.5, 2.0)\n\
         \x20   if (drem(5.5, -2.0) != 1.5) return \"fail: 5.5 % -2.0 = \" + drem(5.5, -2.0)\n\
         \x20   if (drem(3.0, 3.0) != 0.0) return \"fail: 3.0 % 3.0 = \" + drem(3.0, 3.0)\n\
         \x20   if (drem(1.0, 3.0) != 1.0) return \"fail: 1.0 % 3.0 = \" + drem(1.0, 3.0)\n\
         \x20   if (drem(1.0E16, 3.0) != 1.0) return \"fail: 1.0E16 % 3.0 = \" + drem(1.0E16, 3.0)\n\
         \x20   if (drem(1.0, 0.0).toString() != \"NaN\") return \"fail: 1.0 % 0.0\"\n\
         \x20   if (drem(1.0 / 0.0, 2.0).toString() != \"NaN\") return \"fail: inf % 2.0\"\n\
         \x20   if (drem(2.0, 1.0 / 0.0) != 2.0) return \"fail: 2.0 % inf\"\n\
         \x20   if (drem(Double.MIN_VALUE * 3, Double.MIN_VALUE * 2) != Double.MIN_VALUE) return \"fail: subnormal\"\n\
         \x20   if (frem(5.5f, 2.0f) != 1.5f) return \"fail: 5.5f % 2.0f = \" + frem(5.5f, 2.0f)\n\
         \x20   if (frem(-5.5f, 2.0f) != -1.5f) return \"fail: -5.5f % 2.0f\"\n\
         \x20   if (frem(1.0f, 0.0f).toString() != \"NaN\") return \"fail: 1.0f % 0.0f\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "FloatRem",
    );
}
