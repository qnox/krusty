//! Membership in the stdlib's `indices` of an array, a `Collection` or a `CharSequence` is
//! compared like kotlinc's `RangeContainsLowering` over an `IndicesHandler` header:
//! `i in xs.indices` is `0 <= i && i < size`, with `size` read through the receiver class's own
//! `size` or `length` (the receiver cast to that class when its static type is another one) and
//! the comparison widened to the value's type. Neither `indices` nor `contains` is called. A value
//! or receiver with side effects keeps its evaluation order; a nullable value, a written
//! `contains` call and a user `indices` property call `contains` as written.

use super::common;

const FUNCTIONS: &str = r#"
fun ints(xs: IntArray, i: Int) = i in xs.indices
fun objs(xs: Array<String>, i: Int) = i !in xs.indices
fun list(xs: List<Int>, i: Int): Int { if (i in xs.indices) return 1; return 0 }
fun coll(xs: Collection<Int>, i: Int) = when (i) { in xs.indices -> 1; else -> 0 }
fun notColl(xs: Collection<Int>, i: Int) = when (i) { !in xs.indices -> 1; else -> 0 }
fun text(s: String, i: Int) = i in s.indices
fun chars(s: CharSequence, i: Int) = when { i !in s.indices -> 1; else -> 0 }
fun asserted(xs: IntArray?, i: Int) = i in xs!!.indices
fun wide(xs: List<Int>, i: Long) = i in xs.indices
fun both(xs: IntArray, s: String, i: Int, j: Int): Boolean { val b = i in xs.indices && j !in s.indices; return b }
var calls = ""
fun next(): Int { calls += "n"; return 1 }
fun make(): IntArray { calls += "m"; return intArrayOf(1, 2) }
fun ordered() = next() in make().indices
fun written(xs: IntArray, i: Int) = xs.indices.contains(i)
fun boxed(xs: IntArray, i: Int?) = i in xs.indices
"#;

const BOX: &str = r#"
class Holder(val n: Int)
val Holder.indices: IntRange get() = 0 until n

fun user(h: Holder, i: Int) = i in h.indices

fun box(): String {
    val results = listOf(
        ints(intArrayOf(1, 2), 1), ints(intArrayOf(1, 2), 2), ints(intArrayOf(1, 2), -1),
        objs(arrayOf("a"), 0), objs(arrayOf("a"), 1), list(listOf(1), 0), list(listOf(1), 1),
        coll(setOf(1, 2), 1) == 1, coll(setOf(1, 2), 5) == 1, notColl(setOf(1), 3) == 1,
        text("ab", 1), text("ab", 2), chars(StringBuilder("ab"), 2) == 1, asserted(intArrayOf(1), 0),
        wide(listOf(1, 2), 1L), wide(listOf(1, 2), 4_000_000_000L), both(intArrayOf(1), "ab", 0, 2),
        ordered(), written(intArrayOf(1), 0), boxed(intArrayOf(1), null), boxed(intArrayOf(1), 0),
        user(Holder(2), 1), user(Holder(2), 2),
    ).map { if (it == true || it == 1) 1 else 0 }.joinToString("")
    return if (results == "10001101011011101110110" && calls == "mn") "OK" else "$results $calls"
}
"#;

#[test]
fn indices_membership_is_byte_identical_to_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "IndicesContains",
        FUNCTIONS,
        &["IndicesContainsKt"],
    );
}

#[test]
fn indices_membership_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(&format!("{FUNCTIONS}{BOX}"), "indices_contains");
}
