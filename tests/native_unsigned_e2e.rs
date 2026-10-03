//! Kotlin's unsigned integers through krusty's own code generator and runtime.
//!
//! Each of the four is a value class over a signed primitive, and common lowering erases it to the
//! machine integer it wraps before the backend sees it. That erasure is right about the bits and
//! silent about how to read them: `4294967295u` and `-1` are the same 32 bits. Every program here
//! is one of the questions whose answer depends on reading them one way rather than the other, so
//! each would pass by accident if the value were simply carried as the signed number — and each was
//! why the whole family was declined until it could be answered.

use super::common::{expect_native_box, expect_native_decline};

#[test]
fn an_unsigned_value_renders_as_the_value_and_not_as_its_bits() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val a: UInt = 4294967295u\n\
         \x20   if (a.toString() != \"4294967295\") return \"fail toString: \" + a.toString()\n\
         \x20   if (\"$a\" != \"4294967295\") return \"fail template: $a\"\n\
         \x20   if (\"\" + a != \"4294967295\") return \"fail plus: \" + (\"\" + a)\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedRendering",
        "OK",
    );
}

#[test]
fn every_unsigned_width_renders_its_own_maximum() {
    // `UByte` and `UShort` reach the generator already widened into an `Int`, with the checked type
    // saying how narrow they really are; read at the wrong width each renders `-1`.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val joined = \"\" + UByte.MAX_VALUE + \"|\" + UShort.MAX_VALUE + \"|\" +\n\
         \x20       UInt.MAX_VALUE + \"|\" + ULong.MAX_VALUE\n\
         \x20   val expected = \"255|65535|4294967295|18446744073709551615\"\n\
         \x20   return if (joined == expected) \"OK\" else \"fail: $joined\"\n\
         }\n",
        "UnsignedMaxima",
        "OK",
    );
}

#[test]
fn an_unsigned_comparison_reads_the_top_bit_as_a_value() {
    // The whole difference in one program: as signed numbers `0 >= -1` is true, and as the unsigned
    // values they stand for `0u >= ULong.MAX_VALUE` is false.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val max = 0xFFFF_FFFF_FFFF_FFFFuL\n\
         \x20   val zero = 0uL\n\
         \x20   if (zero >= max) return \"fail ge\"\n\
         \x20   if (max <= zero) return \"fail le\"\n\
         \x20   if (!(zero < max)) return \"fail lt\"\n\
         \x20   if (zero.compareTo(max) >= 0) return \"fail compareTo\"\n\
         \x20   val big: UInt = 3000000000u\n\
         \x20   if (big < 1u) return \"fail uint compare\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedComparison",
        "OK",
    );
}

#[test]
fn unsigned_division_and_remainder_are_unsigned() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val a: UInt = 4294967295u\n\
         \x20   if (a / 2u != 2147483647u) return \"fail div: \" + (a / 2u)\n\
         \x20   if (a % 10u != 5u) return \"fail rem: \" + (a % 10u)\n\
         \x20   val l: ULong = 18446744073709551615uL\n\
         \x20   if (l / 2uL != 9223372036854775807uL) return \"fail long div: \" + (l / 2uL)\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedDivision",
        "OK",
    );
}

#[test]
fn unsigned_arithmetic_wraps_the_way_the_machine_does() {
    // Two's complement makes these the same bits either way, which is exactly why they are worth
    // pinning: a wrong answer here would mean the operands were widened by sign.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val a: UInt = 4294967295u\n\
         \x20   if (a + 1u != 0u) return \"fail wrap: \" + (a + 1u)\n\
         \x20   if (0u - 1u != a) return \"fail borrow\"\n\
         \x20   if (a * 2u != 4294967294u) return \"fail times: \" + (a * 2u)\n\
         \x20   val b: UByte = 255u\n\
         \x20   if ((b + b).toString() != \"510\") return \"fail ubyte plus: \" + (b + b)\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedArithmetic",
        "OK",
    );
}

#[test]
fn an_unsigned_conversion_widens_by_zero_extension() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val a: UInt = 4294967295u\n\
         \x20   if (a.toLong() != 4294967295L) return \"fail toLong: \" + a.toLong()\n\
         \x20   if (a.toInt() != -1) return \"fail toInt: \" + a.toInt()\n\
         \x20   if (a.toULong().toString() != \"4294967295\") return \"fail toULong\"\n\
         \x20   val b: UByte = 255u\n\
         \x20   if (b.toInt() != 255) return \"fail ubyte toInt: \" + b.toInt()\n\
         \x20   val s: UShort = 65535u\n\
         \x20   if (s.toInt() != 65535) return \"fail ushort toInt: \" + s.toInt()\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedConversion",
        "OK",
    );
}

#[test]
fn unsigned_bitwise_operators_and_shifts() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val u: UInt = 3000000000u\n\
         \x20   if ((u and 0xFFu) != 0u) return \"fail and: \" + (u and 0xFFu)\n\
         \x20   if ((u or 1u) != 3000000001u) return \"fail or: \" + (u or 1u)\n\
         \x20   if ((u xor u) != 0u) return \"fail xor\"\n\
         \x20   if (u.inv() != 1294967295u) return \"fail inv: \" + u.inv()\n\
         \x20   // A right shift of an unsigned value is a LOGICAL one: the top bit is a value.\n\
         \x20   if ((u shr 1) != 1500000000u) return \"fail shr: \" + (u shr 1)\n\
         \x20   if ((1u shl 31) != 2147483648u) return \"fail shl: \" + (1u shl 31)\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedBitwise",
        "OK",
    );
}

#[test]
fn a_boxed_unsigned_value_is_not_the_signed_one_sharing_its_bits() {
    // The descriptor is what separates them: a `UInt` boxed as an `Int` would answer `is Int` true
    // and would print the signed number.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val any: Any = 7u\n\
         \x20   if (any !is UInt) return \"fail: not a UInt\"\n\
         \x20   if (\"$any\" != \"7\") return \"fail render: $any\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "BoxedUnsignedIdentity",
        "OK",
    );
}

#[test]
fn an_unsigned_value_does_not_answer_a_signed_cast() {
    expect_native_box(
        "fun same(x: UInt) = x as? Int\n\
         fun box(): String = if (same(1u) == null) \"OK\" else \"fail\"\n",
        "UnsignedSignedCast",
        "OK",
    );
}

#[test]
fn a_nullable_unsigned_value_boxes_and_unboxes_through_its_own_type() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val a: UInt? = 4294967295u\n\
         \x20   val absent: UInt? = null\n\
         \x20   if (absent != null) return \"fail null\"\n\
         \x20   val value = a ?: 0u\n\
         \x20   if (value.toString() != \"4294967295\") return \"fail value: $value\"\n\
         \x20   if (\"$a\" != \"4294967295\") return \"fail template: $a\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "NullableUnsigned",
        "OK",
    );
}

#[test]
fn two_boxed_unsigned_values_compare_by_the_value_they_stand_for() {
    // `==` between two boxed values is the runtime's structural equality, which reads the
    // descriptor to decide what the bits mean. A `UInt` whose descriptor it did not know would
    // answer `false` to itself.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val a: UInt? = 4294967295u\n\
         \x20   val b: UInt? = 4294967295u\n\
         \x20   val other: UInt? = 1u\n\
         \x20   if (a != b) return \"fail equal\"\n\
         \x20   if (a == other) return \"fail unequal\"\n\
         \x20   val one: Any = 1u\n\
         \x20   val signed: Any = 1\n\
         \x20   if (one == signed) return \"fail cross-type\"\n\
         \x20   // Kotlin defines each unsigned `hashCode` as the wrapped value's.\n\
         \x20   if (4294967295u.hashCode() != (-1).hashCode()) return \"fail hash\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "BoxedUnsignedEquality",
        "OK",
    );
}

/// A SIGNED value converted to an unsigned type — `42.toUInt()`, `(-1).toUByte()`.
///
/// These are not members of an unsigned type: the receiver is signed, so they live on the facade
/// beside the value class and the unsigned member table never saw them. Every one declined, which
/// on this corpus was 21 cases across `toUInt` and `toUByte` alone.
///
/// Kotlin defines each as the ordinary signed conversion to the target's width with those bits
/// reinterpreted, and the cases below are the ones where that rule is least obvious — so a
/// backend that guessed the conversion's signedness from the TARGET instead of the source would
/// fail them rather than pass by accident:
///
/// * `(-1).toULong()` WIDENS a negative: the source's sign extends, so every bit is set and the
///   answer is `ULong`'s maximum. Zero-extending would answer 4294967295.
/// * `(200.toByte()).toUInt()` widens a `Byte` whose unsigned reading (200) and signed reading
///   (-56) differ; the signed one is what extends.
/// * `300.toUByte()` and `(-1L).toUByte()` NARROW, where the target's width truncates.
///
/// Every expectation is kotlinc's, taken by running the same expressions under it.
#[test]
fn a_signed_value_converts_to_an_unsigned_one_by_its_own_signedness() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   if (42.toUInt() != 42u) return \"fail 42\"\n\
         \x20   if ((-1).toUInt().toString() != \"4294967295\") return \"fail m1 toUInt\"\n\
         \x20   if ((-1).toUByte().toString() != \"255\") return \"fail m1 toUByte\"\n\
         \x20   if ((-1).toUShort().toString() != \"65535\") return \"fail m1 toUShort\"\n\
         \x20   if ((-1).toULong().toString() != \"18446744073709551615\") return \"fail m1 toULong\"\n\
         \x20   if (300.toUByte().toString() != \"44\") return \"fail 300 toUByte\"\n\
         \x20   if ((-1L).toUInt().toString() != \"4294967295\") return \"fail m1L toUInt\"\n\
         \x20   if ((-1L).toUByte().toString() != \"255\") return \"fail m1L toUByte\"\n\
         \x20   if ((200.toByte()).toUByte().toString() != \"200\") return \"fail byte toUByte\"\n\
         \x20   if ((200.toByte()).toUInt().toString() != \"4294967240\") return \"fail byte toUInt\"\n\
         \x20   if (((-2).toShort()).toUShort().toString() != \"65534\") return \"fail short toUShort\"\n\
         \x20   if (((-2).toShort()).toUInt().toString() != \"4294967294\") return \"fail short toUInt\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "SignedToUnsigned",
        "OK",
    );
}

/// A FLOAT converted to an unsigned type still declines, and that is the rule rather than an
/// oversight: `Double.toUInt()` saturates at zero for a negative where the signed conversion
/// reinterpreted would answer a huge positive. It is a different rule, so it is a different change,
/// and this pins that the backend refuses it instead of answering it wrongly.
#[test]
fn a_float_converted_to_an_unsigned_type_is_declined() {
    expect_native_decline(
        "fun box(): String {\n\
         \x20   val d: Double = -1.5\n\
         \x20   return if (d.toUInt() == 0u) \"OK\" else \"fail\"\n\
         }\n",
        "FloatToUnsigned",
        "toUInt",
    );
}
