//! `a.mod(b)` — the remainder carrying the DIVISOR's sign, where `%` carries the dividend's.
//!
//! The two disagree on exactly the operands whose signs differ, which is the whole reason Kotlin
//! declares both: `(-7) % 3` is `-1` and `(-7).mod(3)` is `2`. A backend that answered `%` for
//! both would be right on more than half the inputs and wrong on the rest, so every expectation
//! below is a pair — what `%` says beside what `mod` says.
//!
//! On floating point the sign that decides is Kotlin's `sign`, not the sign bit: it answers NaN
//! for NaN, and a NaN sign compares unequal to everything, which is what carries a NaN out of the
//! adjustment rather than into an addition that would hide it. Both zeros are answered as they
//! stand, because `r != 0.0` is false for either.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// The integers, at each width and across them.
#[test]
fn an_integer_mod_carries_the_divisors_sign() {
    let source = "fun box(): String {\n\
         \x20   if (7 % 3 != 1 || 7.mod(3) != 1) return \"fail both positive\"\n\
         \x20   if ((-7) % 3 != -1) return \"fail negative rem\"\n\
         \x20   if ((-7).mod(3) != 2) return \"fail negative mod\"\n\
         \x20   if (7 % (-3) != 1) return \"fail negative divisor rem\"\n\
         \x20   if (7.mod(-3) != -2) return \"fail negative divisor mod\"\n\
         \x20   if ((-7).mod(-3) != -1) return \"fail both negative\"\n\
         \x20   if (6.mod(3) != 0) return \"fail exact\"\n\
         \x20   if ((-6).mod(3) != 0) return \"fail exact negative\"\n\
         \x20   val b: Byte = (-7).toByte()\n\
         \x20   if (b.mod(3.toByte()) != 2.toByte()) return \"fail Byte\"\n\
         \x20   val s: Short = (-7).toShort()\n\
         \x20   if (s.mod(3.toShort()) != 2.toShort()) return \"fail Short\"\n\
         \x20   if ((-7L).mod(3L) != 2L) return \"fail Long\"\n\
         \x20   // Mixed widths meet in the wider type, so this is a `Long` question.\n\
         \x20   if ((-7).mod(3L) != 2L) return \"fail Int mod Long\"\n\
         \x20   if (Int.MIN_VALUE.mod(-1) != 0) return \"fail MIN_VALUE\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "IntegerFloorMod");
    expect_native_box(source, "IntegerFloorMod", "OK");
}

/// A zero divisor throws, because `mod` is `%` adjusted and `%` throws.
#[test]
fn an_integer_mod_by_zero_throws_the_arithmetic_exception() {
    let source = "fun box(): String {\n\
         \x20   try {\n\
         \x20       7.mod(0)\n\
         \x20       return \"fail: mod by zero returned\"\n\
         \x20   } catch (e: ArithmeticException) {\n\
         \x20       return \"OK\"\n\
         \x20   }\n\
         }\n";
    expect_box_ok_with_stdlib(source, "FloorModByZero");
    expect_native_box(source, "FloorModByZero", "OK");
}

/// Floating point, including the values where the sign that decides is not a bit.
#[test]
fn a_floating_point_mod_carries_the_divisors_sign_and_keeps_a_nan() {
    let source = "fun signOf(x: Double): Int {\n\
         \x20   if (x.isNaN()) return 0\n\
         \x20   if (x == 0.0) return if (1.0 / x == Double.NEGATIVE_INFINITY) -1 else 1\n\
         \x20   return if (x > 0) 1 else -1\n\
         }\n\
         fun box(): String {\n\
         \x20   if (10.125.mod(-0.5) != -0.375) return \"fail negative divisor: \" + 10.125.mod(-0.5)\n\
         \x20   if ((-10.125).mod(0.5) != 0.375) return \"fail negative dividend: \" + (-10.125).mod(0.5)\n\
         \x20   if (10.125.mod(0.5) != 0.125) return \"fail both positive\"\n\
         \x20   if ((-10.125).mod(-0.5) != -0.125) return \"fail both negative\"\n\
         \x20   if (signOf(10.125.mod(-0.5)) != signOf(-0.5)) return \"fail sign follows divisor\"\n\
         \x20   // A zero divisor is NaN rather than a throw, which is what `%` answers too.\n\
         \x20   if (!1.234.mod(0.0).isNaN()) return \"fail zero divisor\"\n\
         \x20   if (!1.234.mod(Double.NaN).isNaN()) return \"fail NaN divisor\"\n\
         \x20   if (!Double.NaN.mod(1.0).isNaN()) return \"fail NaN dividend\"\n\
         \x20   if (10.0f.mod(-0.5f) != -0.0f) return \"fail Float exact\"\n\
         \x20   if ((-10.25f).mod(0.5f) != 0.25f) return \"fail Float\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "FloatingFloorMod");
    expect_native_box(source, "FloatingFloorMod", "OK");
}
