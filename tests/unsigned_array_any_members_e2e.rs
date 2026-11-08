//! An unsigned array's `toString` and `hashCode` in the shape of kotlinc's JVM backend.
//!
//! An unsigned array is a value class over the signed array of its width, so a call carries the
//! signed array unboxed. kotlinc realizes the generated `toString` and `hashCode` over that carrier
//! as the class's statics, `UIntArray.toString-impl([I)` and `UIntArray.hashCode-impl([I)`. A
//! virtual call on the box's `toString()` would dispatch on a `[I` the verifier cannot accept as a
//! `kotlin/UIntArray`. A template renders through the same static; an `Any`-typed value is boxed
//! and dispatches virtually.

use super::common;

const DECLARATIONS: &str = r#"@file:OptIn(ExperimentalUnsignedTypes::class)

fun bytes(a: UByteArray): String = a.toString()
fun shorts(a: UShortArray): String = a.toString()
fun ints(a: UIntArray): String = a.toString()
fun longs(a: ULongArray): String = a.toString()
fun joined(a: UIntArray): String = "storage " + a.toString()
fun template(b: UByteArray, s: UShortArray, i: UIntArray, l: ULongArray): String = "$b|$s|$i|$l"
fun boxed(a: UIntArray): String { val any: Any = a; return any.toString() }
fun hashes(b: UByteArray, s: UShortArray, i: UIntArray, l: ULongArray): Int =
    b.hashCode() + s.hashCode() + i.hashCode() + l.hashCode()
"#;

const BOX: &str = r#"
fun box(): String {
    val b = ubyteArrayOf(1u, 200u)
    val s = ushortArrayOf(1u, 65535u)
    val i = uintArrayOf(1u, 4000000000u)
    val l = ulongArrayOf(1uL, 18446744073709551615uL)
    val texts = listOf(
        bytes(b),
        shorts(s),
        ints(i),
        longs(l),
        joined(i),
        template(b, s, i, l),
        boxed(i),
        (hashes(b, s, i, l) == hashes(b, s, i, l)).toString(),
        (i.hashCode() == (i as Any).hashCode()).toString(),
    ).joinToString(",")
    val expected = "UByteArray(storage=[1, -56]),UShortArray(storage=[1, -1])," +
        "UIntArray(storage=[1, -294967296]),ULongArray(storage=[1, -1])," +
        "storage UIntArray(storage=[1, -294967296])," +
        "UByteArray(storage=[1, -56])|UShortArray(storage=[1, -1])|" +
        "UIntArray(storage=[1, -294967296])|ULongArray(storage=[1, -1])," +
        "UIntArray(storage=[1, -294967296]),true,true"
    return if (texts == expected) "OK" else texts
}
"#;

#[test]
fn an_unsigned_array_renders_and_hashes_through_its_statics_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "UnsignedArrayAnyMembers",
        DECLARATIONS,
        &["UnsignedArrayAnyMembersKt"],
    );
}

#[test]
fn an_unsigned_array_renders_and_hashes_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        &format!("{DECLARATIONS}{BOX}"),
        "unsigned_array_any_members",
    );
}
