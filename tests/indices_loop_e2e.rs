//! A `for` loop over the stdlib's `indices` of an array, a `Collection` or a `CharSequence` is
//! counted like kotlinc's `IndicesHandler`: `for (i in xs.indices)` is `0 until size` with `size`
//! read once through the receiver class's own `size` (`Collection.size`, `CharSequence.length`,
//! an array's length), the receiver cast to that class when its static type is another one. No
//! `IntRange` is built. Under `reversed()` or `step` the bound is the inclusive `size + -1`.

use super::common;

const FUNCTIONS: &str = r#"
fun ints(xs: IntArray): Int { var s = 0; for (i in xs.indices) s += i; return s }
fun objs(xs: Array<String>): Int { var s = 0; for (i in xs.indices) s += i; return s }
fun list(xs: List<Int>): Int { var s = 0; for (i in xs.indices) s += xs[i]; return s }
fun coll(xs: Collection<String>): Int { var s = 0; for (i in xs.indices) s += i; return s }
fun text(xs: String): Int { var s = 0; for (i in xs.indices) s += i; return s }
fun chars(xs: CharSequence): Int { var s = 0; for (i in xs.indices) s += i; return s }
fun rev(xs: IntArray): String { var s = ""; for (i in xs.indices.reversed()) s += i; return s }
fun stepped(xs: IntArray): String { var s = ""; for (i in xs.indices step 2) s += i; return s }
fun revList(xs: List<String>): String { var s = ""; for (i in xs.indices.reversed()) s += xs[i]; return s }
fun call(): Int { var s = 0; for (i in make().indices) s += i; return s }
fun make() = intArrayOf(1, 2, 3)
fun nullable(xs: IntArray?): Int { var s = 0; if (xs != null) for (i in xs.indices) s += i; return s }
fun mutable(xs: MutableList<Int>): Int { var s = 0; for (i in xs.indices) s += i; return s }
fun emptyLoop(xs: LongArray): Int { var n = 0; for (i in xs.indices) n++; return n }
"#;

const BOX: &str = r#"
class Holder(val n: Int)
val Holder.indices: IntRange get() = 0 until n

fun user(h: Holder): Int { var s = 0; for (i in h.indices) s += i; return s }

fun box(): String {
    val results = listOf(
        ints(intArrayOf(5, 5, 5, 5)), objs(arrayOf("a", "b")), list(listOf(4, 5, 6)),
        coll(setOf("x", "y", "z")), text("abcd"), chars(StringBuilder("ab")), call(),
        nullable(null), nullable(intArrayOf(0, 0, 0)), mutable(mutableListOf(1, 1)),
        emptyLoop(LongArray(0)), user(Holder(4)),
    )
    val texts = listOf(rev(intArrayOf(9, 9, 9)), stepped(IntArray(5)), revList(listOf("a", "b", "c")))
    return if (results == listOf(6, 1, 15, 3, 6, 1, 3, 0, 3, 1, 0, 6) && texts == listOf("210", "024", "cba")) "OK"
    else "$results $texts"
}
"#;

#[test]
fn an_indices_loop_is_byte_identical_to_kotlinc() {
    common::assert_classes_identical_to_kotlinc("IndicesLoop", FUNCTIONS, &["IndicesLoopKt"]);
}

#[test]
fn an_indices_loop_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(&format!("{FUNCTIONS}{BOX}"), "indices_loop");
}
