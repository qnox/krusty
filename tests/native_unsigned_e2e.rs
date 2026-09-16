//! Kotlin's unsigned integers through krusty's own code generator and runtime.
//!
//! Each of the four is a value class over a signed primitive, and common lowering erases it to the
//! machine integer it wraps before the backend sees it. That erasure is right about the bits and
//! silent about how to read them: `4294967295u` and `-1` are the same 32 bits. Every program here
//! is one of the questions whose answer depends on reading them one way rather than the other, so
//! each would pass by accident if the value were simply carried as the signed number — and each was
//! why the whole family was declined until it could be answered.

use super::common::expect_native_box;

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

/// The four unsigned ARRAYS, each at the top value of its width.
///
/// An unsigned array is a value class over the signed array of the same width, so the elements are
/// the signed array's bits and only the reading of them is unsigned — the same erasure the scalars
/// above go through, one level out. Every element here is the one a SIGNED read of the same bits
/// answers as `-1`, so a wrong stride or a signed load cannot be right by accident.
#[test]
fn every_unsigned_array_width_reads_back_what_it_stored() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val b = UByteArray(2); b[0] = 255u; b[1] = 7u\n\
         \x20   val s = UShortArray(2); s[0] = 65535u; s[1] = 7u\n\
         \x20   val i = UIntArray(2); i[0] = 4294967295u; i[1] = 7u\n\
         \x20   val l = ULongArray(2); l[0] = 18446744073709551615uL; l[1] = 7u\n\
         \x20   if (b[0].toString() != \"255\") return \"fail ubyte \" + b[0].toString()\n\
         \x20   if (s[0].toString() != \"65535\") return \"fail ushort \" + s[0].toString()\n\
         \x20   if (i[0].toString() != \"4294967295\") return \"fail uint \" + i[0].toString()\n\
         \x20   if (l[0].toString() != \"18446744073709551615\") return \"fail ulong \" + l[0].toString()\n\
         \x20   if (b[1].toString() != \"7\" || s[1].toString() != \"7\") return \"fail second\"\n\
         \x20   if (i[1].toString() != \"7\" || l[1].toString() != \"7\") return \"fail second\"\n\
         \x20   if (b.size != 2 || s.size != 2 || i.size != 2 || l.size != 2) return \"fail size\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedArrays",
        "OK",
    );
}

/// A stride is not a name: an unsigned array carries its OWN type, not the signed array's.
///
/// `UIntArray` and `IntArray` hold the same bits four at a time, so sharing one runtime descriptor
/// would make every program above pass — and would answer `is IntArray` with `true`, where the
/// reference compiler answers `false` because the two are distinct classes. Each unsigned width
/// therefore gets its own descriptor with the signed one's stride.
///
/// NOTE: krusty's JVM backend answers `true` here, which is a separate known defect — it does not
/// box a value class at the `Any` boundary (`docs/BUILD_AND_NATIVE_PLAN.md`). The native answer is
/// the reference compiler's.
#[test]
fn an_unsigned_array_is_not_the_signed_array_it_is_laid_out_as() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val u: Any = UIntArray(1)\n\
         \x20   if (u is IntArray) return \"fail: a UIntArray answered as an IntArray\"\n\
         \x20   if (u !is UIntArray) return \"fail: a UIntArray did not answer as itself\"\n\
         \x20   val i: Any = IntArray(1)\n\
         \x20   if (i is UIntArray) return \"fail: an IntArray answered as a UIntArray\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedArrayIdentity",
        "OK",
    );
}

/// Walking an unsigned array through the ITERATOR protocol, which is where its descriptor stops
/// being the receiver of an indexed load and becomes the only record of how wide an element is.
///
/// `withIndex()` is lazy and general: it wraps the array in an iterable and asks that for elements
/// one at a time, so each element is read by the runtime from the array's descriptor alone rather
/// than by generated code that still knows the static type. A descriptor the reader does not
/// recognise falls through to whatever the last arm of the chain happens to be — which reads a
/// four-byte element eight bytes wide, and answers the next element, or garbage past the end, for
/// values the indexed path gets right. Every element here is distinct and in range, so a wrong
/// stride shows up as a shifted or invented value rather than as a crash.
#[test]
fn an_unsigned_array_walks_at_its_own_width() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   var s = \"\"\n\
         \x20   for ((i, u) in ubyteArrayOf(1u, 2u, 255u).withIndex()) s += \"$i:$u;\"\n\
         \x20   if (s != \"0:1;1:2;2:255;\") return \"fail ubyte: $s\"\n\
         \x20   s = \"\"\n\
         \x20   for ((i, u) in ushortArrayOf(1u, 2u, 65535u).withIndex()) s += \"$i:$u;\"\n\
         \x20   if (s != \"0:1;1:2;2:65535;\") return \"fail ushort: $s\"\n\
         \x20   s = \"\"\n\
         \x20   for ((i, u) in uintArrayOf(1u, 2u, 4294967295u).withIndex()) s += \"$i:$u;\"\n\
         \x20   if (s != \"0:1;1:2;2:4294967295;\") return \"fail uint: $s\"\n\
         \x20   s = \"\"\n\
         \x20   for ((i, u) in ulongArrayOf(1u, 2u, 18446744073709551615uL).withIndex()) s += \"$i:$u;\"\n\
         \x20   if (s != \"0:1;1:2;2:18446744073709551615;\") return \"fail ulong: $s\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedArrayWalk",
        "OK",
    );
}

/// The same walk with nothing counting it: `joinToString` asks the array itself for an iterator.
///
/// This is the shorter route to the same reader — no `IndexedValue` in between — and it is the one
/// that renders each element through its own `toString`, so an element read at the right width but
/// as the SIGNED number of those bits renders as `-1` rather than as the maximum.
#[test]
fn an_unsigned_array_renders_each_element_unsigned_when_it_is_walked() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val b = ubyteArrayOf(255u).joinToString()\n\
         \x20   if (b != \"255\") return \"fail ubyte: $b\"\n\
         \x20   val s = ushortArrayOf(65535u).joinToString()\n\
         \x20   if (s != \"65535\") return \"fail ushort: $s\"\n\
         \x20   val i = uintArrayOf(4294967295u).joinToString()\n\
         \x20   if (i != \"4294967295\") return \"fail uint: $i\"\n\
         \x20   val l = ulongArrayOf(18446744073709551615uL).joinToString()\n\
         \x20   if (l != \"18446744073709551615\") return \"fail ulong: $l\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedArrayJoin",
        "OK",
    );
}
