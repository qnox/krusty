//! An unsigned array is allocated through the declarations its class and library declare, as
//! kotlinc allocates it.
//!
//! `UIntArray` is a value class, not a compiler-provided array: `UIntArray(n)` is its declared
//! `constructor(size: Int)`, which kotlinc calls (`UIntArray.constructor-impl(I)[I`), and
//! `UIntArray(n) { init }` is the library's inline function, whose body allocates the signed
//! carrier, fills it through the inlined initializer and wraps it with the primary constructor
//! (`constructor-impl([I)[I`). The initializer's result arrives boxed through `Function1` and is
//! unboxed straight away, so kotlinc's redundant-boxing pass removes both, as it does for any
//! value class.

use super::common;

const DECLARATIONS: &str = r#"@file:OptIn(ExperimentalUnsignedTypes::class)

class Grid(n: Int) {
    val cells = UIntArray(n)
    val marks: Any = UShortArray(n)
}

fun ints(n: Int): UIntArray = UIntArray(n)
fun bytes(n: Int): UByteArray = UByteArray(n)
fun shorts(n: Int): UShortArray = UShortArray(n)
fun longs(n: Int): ULongArray = ULongArray(n)
fun boxed(n: Int): Any = UIntArray(n)
fun nullable(n: Int): UIntArray? = if (n > 0) UIntArray(n) else null
fun sized(n: Int): Int = UByteArray(n + 1).size
fun filled(n: Int, seed: UInt): UIntArray = UIntArray(n) { seed }
fun constant(n: Int): ULongArray = ULongArray(n) { 9uL }
fun byteConstant(n: Int): UByteArray = UByteArray(n) { 255u }
fun shortCapture(n: Int, seed: UShort): UShortArray = UShortArray(n) { seed }

typealias Cells = UIntArray

fun aliased(n: Int): Cells = Cells(n)
"#;

const BOX: &str = r#"
fun indexed(n: Int): UIntArray = UIntArray(n) { (it * 2).toUInt() }
fun wrapped(n: Int): UByteArray = UByteArray(n) { (it - 1).toUByte() }

fun box(): String {
    val grid = Grid(2)
    val texts = listOf(
        ints(3).size.toString(),
        bytes(2).size.toString(),
        shorts(1).size.toString(),
        longs(4).size.toString(),
        (boxed(2) is UIntArray).toString(),
        nullable(0).toString(),
        nullable(1)!!.size.toString(),
        sized(2).toString(),
        filled(2, 4000000000u)[1].toString(),
        constant(2)[0].toString(),
        byteConstant(1)[0].toString(),
        shortCapture(2, 65535u)[1].toString(),
        aliased(5).size.toString(),
        grid.cells.size.toString(),
        (grid.marks is UShortArray).toString(),
        indexed(3).joinToString("/"),
        wrapped(2).joinToString("/"),
    ).joinToString(",")
    val expected = "3,2,1,4,true,null,1,3,4000000000,9,255,65535,5,2,true,0/2/4,255/0"
    return if (texts == expected) "OK" else texts
}
"#;

#[test]
fn an_unsigned_array_is_allocated_through_its_declarations_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "UnsignedAllocation",
        DECLARATIONS,
        &["UnsignedAllocationKt", "Grid"],
    );
}

#[test]
fn an_unsigned_array_allocation_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        &format!("{DECLARATIONS}{BOX}"),
        "unsigned_array_allocation",
    );
}
