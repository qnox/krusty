//! A cast to an unsigned array checks its box and unboxes it, as kotlinc casts any value class.
//!
//! `UIntArray` is a value class over `IntArray`, and the two share the descriptor `[I`. A cast
//! from a reference (`a as UIntArray`, `a as UIntArray?`, `a as? UIntArray`) tests the BOX:
//! `checkcast kotlin/UIntArray`, then `unbox-impl`. The written cast is that box test itself, so
//! no second cast is added for the unbox to read; a cast naming the carrier (`checkcast [I`)
//! would throw for every real `UIntArray`. A safe cast unboxes where it succeeds, inside its
//! `when`, and its `null` branch is already the carrier's `null`; this holds for any value class
//! (`Bits` below), which is why the shape is compared for both.

use super::common;

const DECLARATIONS: &str = r#"@file:OptIn(ExperimentalUnsignedTypes::class)

@JvmInline
value class Bits(val raw: IntArray)

fun ints(a: Any): UIntArray = a as UIntArray
fun bytes(a: Any): Int = (a as UByteArray).size
fun shorts(a: Any): UShortArray = a as UShortArray
fun longs(a: Any): ULong = (a as ULongArray)[0]
fun nullableCast(a: Any?): UIntArray? = a as UIntArray?
fun safeInts(a: Any?): UIntArray? = a as? UIntArray
fun safeLongs(a: Any?): Int = (a as? ULongArray)?.size ?: -1
fun produced(): Any = ubyteArrayOf(7u)
fun safeProduced(): UByteArray? = produced() as? UByteArray
fun stored(a: Any?): Int {
    val shorts = a as? UShortArray
    return shorts?.size ?: 0
}
fun smart(a: Any): Int = if (a is UIntArray) a.size else -1
fun bits(a: Any): Bits = a as Bits
fun safeBits(a: Any?): Bits? = a as? Bits
"#;

const BOX: &str = r#"
fun box(): String {
    val signed: Any = intArrayOf(1)
    val texts = listOf(
        ints(uintArrayOf(4000000000u))[0].toString(),
        bytes(ubyteArrayOf(1u, 2u)).toString(),
        shorts(ushortArrayOf(65535u))[0].toString(),
        longs(ulongArrayOf(18446744073709551615uL)).toString(),
        nullableCast(null).toString(),
        nullableCast(uintArrayOf(5u))!![0].toString(),
        safeInts(uintArrayOf(6u))!![0].toString(),
        safeInts(signed).toString(),
        safeInts(null).toString(),
        safeLongs(ulongArrayOf(1uL, 2uL)).toString(),
        safeLongs("text").toString(),
        safeProduced()!![0].toString(),
        stored(ushortArrayOf(1u, 2u, 3u)).toString(),
        stored(signed).toString(),
        smart(uintArrayOf(1u, 2u)).toString(),
        smart(signed).toString(),
        bits(Bits(intArrayOf(8))).raw[0].toString(),
        safeBits(Bits(intArrayOf(9)))!!.raw[0].toString(),
        safeBits(signed).toString(),
        runCatching { ints(signed) }.exceptionOrNull()?.javaClass?.simpleName.toString(),
    ).joinToString(",")
    val expected = "4000000000,2,65535,18446744073709551615,null,5,6,null,null,2,-1,7,3,0,2,-1,8,9,null," +
        "ClassCastException"
    return if (texts == expected) "OK" else texts
}
"#;

#[test]
fn a_cast_to_an_unsigned_array_checks_its_box_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "UnsignedCasts",
        DECLARATIONS,
        &["UnsignedCastsKt"],
    );
}

#[test]
fn a_cast_to_an_unsigned_array_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(&format!("{DECLARATIONS}{BOX}"), "unsigned_array_casts");
}
