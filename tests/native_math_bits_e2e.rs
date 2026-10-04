//! `kotlin.math.abs`, the floating-point bit conversions, and iterating a `StringBuilder`.
//!
//! Each is exact and each has one detail that a naive realization gets wrong:
//!
//! - `abs` on an integral type WRAPS at the minimum, because there is no positive value to answer
//!   with. On a floating one it clears the SIGN BIT rather than negating: `-0.0 < 0.0` is false, so
//!   `if (x < 0) -x else x` hands back the negative zero it was given.
//! - `toBits` collapses every NaN to the canonical one where `toRawBits` does not — the same
//!   collapse `equals` and `hashCode` make.
//! - A BUILDER is walkable text, and the chars iterator re-reads its length every step. That is
//!   what lets a loop which shortens the builder stop where Kotlin's stops.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// `abs` at each width, with the minimum and the negative zero that separate the two rules.
#[test]
fn abs_wraps_at_the_integral_minimum_and_clears_the_floating_sign() {
    let source = "import kotlin.math.abs\n\
         fun box(): String {\n\
         \x20   if (abs(-3) != 3 || abs(3) != 3 || abs(0) != 0) return \"fail int\"\n\
         \x20   // No positive value to answer with, so Kotlin answers the minimum itself.\n\
         \x20   if (abs(Int.MIN_VALUE) != Int.MIN_VALUE) return \"fail int wrap\"\n\
         \x20   if (abs(Long.MIN_VALUE) != Long.MIN_VALUE) return \"fail long wrap\"\n\
         \x20   if (abs(-3L) != 3L) return \"fail long\"\n\
         \x20   if (abs(-3.5) != 3.5 || abs(-3.5f) != 3.5f) return \"fail floating\"\n\
         \x20   // `-0.0 == 0.0`, so the sign has to be read from the BITS to see the difference.\n\
         \x20   if (1.0 / abs(-0.0) != Double.POSITIVE_INFINITY) return \"fail minus zero\"\n\
         \x20   if (1.0f / abs(-0.0f) != Float.POSITIVE_INFINITY) return \"fail minus zero float\"\n\
         \x20   if (!abs(Double.NaN).isNaN()) return \"fail nan\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "MathAbs");
    expect_native_box(source, "MathAbs", "OK");
}

/// The bits of a floating-point value, with the one respect `toBits` and `toRawBits` differ in.
///
/// `fromBits` is deliberately absent. It is an extension of the COMPANION object, and while a klib
/// call reaches it with that object unread, a jar call materializes the object first — which this
/// target cannot yet do for a type declared in no file. So `fromBits` is answered in one lane only,
/// and a test pinning it would pass for a reason that is not the one it claims.
#[test]
fn the_bit_conversions_round_trip_and_to_bits_canonicalizes_nan() {
    let source = "fun box(): String {\n\
         \x20   if (1.0f.toRawBits() != 1065353216) return \"fail float raw\"\n\
         \x20   if (1.0.toRawBits() != 4607182418800017408L) return \"fail double raw\"\n\
         \x20   // The sign bit is the whole of what tells the two zeroes apart.\n\
         \x20   if ((-0.0).toRawBits() != Long.MIN_VALUE) return \"fail minus zero\"\n\
         \x20   if (0.0.toRawBits() != 0L) return \"fail zero\"\n\
         \x20   // `toBits` answers ONE NaN whatever NaN it was given; `toRawBits` does not.\n\
         \x20   if (Double.NaN.toBits() != 9221120237041090560L) return \"fail nan canonical\"\n\
         \x20   if (Float.NaN.toBits() != 2143289344) return \"fail nan canonical float\"\n\
         \x20   if (1.5.toRawBits() != 4609434218613702656L) return \"fail one and a half\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "FloatingBits");
    expect_native_box(source, "FloatingBits", "OK");
}

/// `assertSame` is IDENTITY where `assertEquals` is equality.
///
/// Native only: `kotlin.test` is not on the cross-check's classpath, which is why every other
/// assertion test here is written the same way. The wording of the failure was read off the
/// REFERENCE toolchain rather than guessed — `Expected <a>, actual <b> is not same.`
#[test]
fn the_identity_assertions_pass_on_the_same_object_only() {
    let source = "import kotlin.test.assertSame\n\
         import kotlin.test.assertNotSame\n\
         fun box(): String {\n\
         \x20   val held = \"x\"\n\
         \x20   assertSame(held, held)\n\
         \x20   assertSame(held, held, \"with a message\")\n\
         \x20   // Equal and NOT the same object, which is the whole of the difference.\n\
         \x20   val built = StringBuilder(\"x\").toString()\n\
         \x20   if (built != held) return \"fail equal\"\n\
         \x20   assertNotSame(held, built)\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_native_box(source, "AssertSame", "OK");
}

/// `setLength` counts UTF-16 units: shorter truncates, longer pads with NUL.
#[test]
fn setting_a_builders_length_truncates_or_pads_with_nul() {
    let source = "fun box(): String {\n\
         \x20   val b = StringBuilder(\"abcd\")\n\
         \x20   b.setLength(2)\n\
         \x20   if (b.toString() != \"ab\") return \"fail truncate\"\n\
         \x20   b.setLength(0)\n\
         \x20   if (b.length != 0 || b.toString() != \"\") return \"fail empty\"\n\
         \x20   b.append(\"xy\")\n\
         \x20   b.setLength(4)\n\
         \x20   if (b.length != 4) return \"fail pad length\"\n\
         \x20   if (b[2] != '\\u0000' || b[3] != '\\u0000') return \"fail pad content\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "BuilderSetLength");
    expect_native_box(source, "BuilderSetLength", "OK");
}
