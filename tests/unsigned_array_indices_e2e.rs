//! A `for` loop over the `indices` of an unsigned array is counted like kotlinc's
//! `ArrayIndicesHandler`, which matches `isUnsignedArray()` under the JVM backend's
//! `optimizeLoopsOverUnsignedArrays`: `for (i in xs.indices)` is `0 until size` with `size` read
//! once through the value class's own `size` member (`UIntArray.getSize-impl(int[])`), and the
//! inclusive `size + -1` under `reversed()` or `step`. No `IntRange` is built. A plain `xs.size`
//! reads the same member, and `xs[i]` in the loop body is the class's own `get`.

use super::common;

const FUNCTIONS: &str = r#"@file:OptIn(ExperimentalUnsignedTypes::class)

fun sumU(xs: UIntArray): Int { var s = 0; for (i in xs.indices) s += i; return s }
fun countL(xs: ULongArray): Int { var n = 0; for (i in xs.indices) n += i; return n }
fun revB(xs: UByteArray): String { var s = ""; for (i in xs.indices.reversed()) s += i; return s }
fun stepS(xs: UShortArray): String { var s = ""; for (i in xs.indices step 2) s += i; return s }
fun emptyU(xs: UIntArray): Int { var n = 0; for (i in xs.indices) n++; return n }
fun sizeU(xs: UIntArray): Int = xs.size
fun sizeB(xs: UByteArray): Int = xs.size
fun elements(xs: UIntArray): UInt { var s = 0u; for (i in xs.indices) s += xs[i]; return s }
fun memberU(xs: UIntArray, i: Int) = i in xs.indices
fun notMemberB(xs: UByteArray, i: Int) = i !in xs.indices
fun memberWhenL(xs: ULongArray, i: Int) = when (i) { in xs.indices -> 1; else -> 0 }
"#;

const BOX: &str = r#"
fun box(): String {
    val ints = listOf(
        sumU(uintArrayOf(1u, 2u, 3u)), countL(ulongArrayOf(7u, 7u, 7u)), emptyU(UIntArray(0)),
        sizeU(UIntArray(4)), sizeB(ubyteArrayOf(1u, 255u)),
    )
    val texts = listOf(
        elements(uintArrayOf(1u, 2u, UInt.MAX_VALUE)).toString(),
        revB(ubyteArrayOf(1u, 2u, 255u)),
        stepS(UShortArray(5)),
    )
    val members = listOf(
        memberU(uintArrayOf(1u, 2u), 1), memberU(uintArrayOf(1u, 2u), 2), memberU(UIntArray(0), 0),
        notMemberB(ubyteArrayOf(1u), -1), notMemberB(ubyteArrayOf(1u), 0),
        memberWhenL(ULongArray(3), 2) == 1, memberWhenL(ULongArray(3), 3) == 1,
    )
    val expectedMembers = listOf(true, false, false, true, false, true, false)
    return if (ints == listOf(3, 3, 0, 4, 2) && texts == listOf("2", "210", "024") &&
        members == expectedMembers) "OK"
    else "$ints $texts $members"
}
"#;

#[test]
fn an_unsigned_array_indices_loop_is_byte_identical_to_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "UnsignedIndices",
        FUNCTIONS,
        &["UnsignedIndicesKt"],
    );
}

#[test]
fn an_unsigned_array_indices_loop_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(&format!("{FUNCTIONS}{BOX}"), "unsigned_array_indices");
}
