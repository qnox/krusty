//! A `for` loop over the `indices` of an unsigned array is counted like kotlinc's
//! `ArrayIndicesHandler`, which matches `isUnsignedArray()` under the JVM backend's
//! `optimizeLoopsOverUnsignedArrays`: `for (i in xs.indices)` is `0 until size` with `size` read
//! once through the value class's own `size` member (`UIntArray.getSize-impl(int[])`), and the
//! inclusive `size + -1` under `reversed()` or `step`. No `IntRange` is built. A plain `xs.size`
//! reads the same member.

use super::common;

const FUNCTIONS: &str = r#"@file:OptIn(ExperimentalUnsignedTypes::class)

fun sumU(xs: UIntArray): Int { var s = 0; for (i in xs.indices) s += i; return s }
fun countL(xs: ULongArray): Int { var n = 0; for (i in xs.indices) n += i; return n }
fun revB(xs: UByteArray): String { var s = ""; for (i in xs.indices.reversed()) s += i; return s }
fun stepS(xs: UShortArray): String { var s = ""; for (i in xs.indices step 2) s += i; return s }
fun emptyU(xs: UIntArray): Int { var n = 0; for (i in xs.indices) n++; return n }
fun sizeU(xs: UIntArray): Int = xs.size
fun sizeB(xs: UByteArray): Int = xs.size
"#;

// Element reads stay out of the byte comparison: krusty still reads them with the carrier's
// `iaload` where kotlinc calls `UIntArray.get-pVg5ArA`.
const BOX: &str = r#"
fun elements(xs: UIntArray): UInt { var s = 0u; for (i in xs.indices) s += xs[i]; return s }

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
    return if (ints == listOf(3, 3, 0, 4, 2) && texts == listOf("2", "210", "024")) "OK"
    else "$ints $texts"
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
