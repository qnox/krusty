//! The members Kotlin declares over `Iterable` that the runtime answers by WALKING, second batch.
//!
//! `native_collection_walks_e2e` pinned the predicates and the accumulations. These are the rest of
//! what a box program reaches for once the receiver is an iterable the runtime cannot index: the
//! ends of the walk, the walk of length one, the transforms that drop elements, and the fold that
//! starts from the first element.
//!
//! The one thing that needed deciding is what a LIST does with the names both tables carry. The
//! walk table is consulted FIRST, so an entry added here shadows `lists::list_symbol` unless the
//! call site holds it back — and for `first`/`last` the two are not the same answer: Kotlin's list
//! form raises `NoSuchElementException("List is empty.")` where the walk form says
//! "Collection is empty.". So the list keeps those names, and the tests below pin both wordings.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// An iterable of the PROGRAM's, so the receiver is one the runtime cannot index and every member
/// below is answered by walking it. A named inner class rather than an object expression, which is
/// the shape the corpus uses.
const COUNTING_UP: &str = r#"
class UpTo(private val last: Int) : Iterable<Int> {
    inner class Walk : Iterator<Int> {
        private var at = 0
        override fun hasNext() = at < last
        override fun next(): Int { at++; return at }
    }
    override fun iterator(): Iterator<Int> = Walk()
}
"#;

/// `find` is `firstOrNull` with a predicate, under the name Kotlin also declares it by.
#[test]
fn a_walk_finds_the_first_element_matching_a_predicate() {
    let source = r#"
fun box(): String {
    val xs = listOf(1, 2, 3, 4)
    if (xs.find { it > 2 } != 3) return "fail hit"
    if (xs.find { it > 9 } != null) return "fail miss"
    if ((1..4).find { it % 2 == 0 } != 2) return "fail range"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_walk_find", source);
}

/// The ends of a walk over an iterable the runtime cannot index.
#[test]
fn a_walk_answers_its_first_and_last_element() {
    let source = format!(
        "{COUNTING_UP}
fun box(): String {{
    val xs = UpTo(4)
    if (xs.first() != 1) return \"fail first\"
    if (xs.last() != 4) return \"fail last\"
    if (UpTo(1).first() != 1) return \"fail single first\"
    if (UpTo(1).last() != 1) return \"fail single last\"
    return \"OK\"
}}
"
    );
    every_backend_agrees_with_kotlinc("native_walk_first_last", &source);
}

/// An EMPTY walk has no first or last, and Kotlin's wording for that is the collection one.
#[test]
fn an_empty_walk_raises_with_the_collection_wording() {
    let source = format!(
        "{COUNTING_UP}
fun message(run: () -> Unit): String =
    try {{ run(); \"none\" }} catch (e: NoSuchElementException) {{ e.message ?: \"null\" }}

fun box(): String {{
    val empty = UpTo(0)
    val first = message {{ empty.first() }}
    if (first != \"Collection is empty.\") return \"fail first \" + first
    val last = message {{ empty.last() }}
    if (last != \"Collection is empty.\") return \"fail last \" + last
    return \"OK\"
}}
"
    );
    every_backend_agrees_with_kotlinc("native_walk_empty_wording", &source);
}

/// The LIST keeps `first`/`last`, whose wording on an empty receiver is Kotlin's other one. This is
/// what the call site's list gate is for: without it the walk above would answer here too.
#[test]
fn an_empty_list_still_raises_with_the_list_wording() {
    let source = r#"
fun message(run: () -> Unit): String =
    try { run(); "none" } catch (e: NoSuchElementException) { e.message ?: "null" }

fun box(): String {
    val empty = listOf<Int>()
    val first = message { empty.first() }
    if (first != "List is empty.") return "fail first " + first
    val last = message { empty.last() }
    if (last != "List is empty.") return "fail last " + last
    if (listOf(7, 8).first() != 7) return "fail first value"
    if (listOf(7, 8).last() != 8) return "fail last value"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_list_empty_wording", source);
}

/// `singleOrNull` answers null for a walk of any length but one — for TWO as much as for none.
#[test]
fn a_walk_answers_its_single_element_or_null() {
    let source = format!(
        "{COUNTING_UP}
fun box(): String {{
    if (UpTo(1).singleOrNull() != 1) return \"fail one\"
    if (UpTo(0).singleOrNull() != null) return \"fail none\"
    if (UpTo(2).singleOrNull() != null) return \"fail two\"
    return \"OK\"
}}
"
    );
    every_backend_agrees_with_kotlinc("native_walk_single_or_null", &source);
}

/// `isEmpty`/`isNotEmpty` asked of a COLLECTION, under their own names rather than `none`/`any`.
///
/// Kotlin declares neither over `Iterable` — `isEmpty` is a member of `Collection` and `isNotEmpty`
/// an extension of it — so the receiver here is typed `Collection`, which is the corpus shape
/// (`codegen/box/when/kt5448.kt` asks it of a `Collection<A>` field). A `List` is not this case: it
/// answers `isEmpty` from its own size.
#[test]
fn a_collection_answers_whether_it_is_empty() {
    let source = r#"
fun box(): String {
    val some: Collection<Int> = listOf(1, 2)
    val none: Collection<Int> = listOf()
    if (some.isEmpty()) return "fail not empty"
    if (!some.isNotEmpty()) return "fail is not empty"
    if (!none.isEmpty()) return "fail empty"
    if (none.isNotEmpty()) return "fail empty not"
    if (listOf(1).isEmpty()) return "fail list"
    if (listOf<Int>().isNotEmpty()) return "fail empty list"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_collection_is_empty", source);
}

/// `mapNotNull` drops the nulls the TRANSFORM answers — the element may be null and still be asked.
#[test]
fn a_walk_maps_and_drops_the_nulls_the_transform_answers() {
    let source = r#"
fun box(): String {
    val xs = listOf(1, 2, 3, 4)
    val even = xs.mapNotNull { if (it % 2 == 0) it * 10 else null }
    if (even != listOf(20, 40)) return "fail even " + even
    if (xs.mapNotNull { null }.isNotEmpty()) return "fail all null"
    val nullable = listOf<Int?>(1, null, 3)
    val kept = nullable.mapNotNull { it }
    if (kept != listOf(1, 3)) return "fail nullable " + kept
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_walk_map_not_null", source);
}

/// `drop` skips a prefix; past the end is empty, and a negative count is an error Kotlin names.
#[test]
fn a_walk_drops_a_prefix() {
    let source = format!(
        "{COUNTING_UP}
fun box(): String {{
    if (UpTo(4).drop(2) != listOf(3, 4)) return \"fail two\"
    if (UpTo(4).drop(0) != listOf(1, 2, 3, 4)) return \"fail none\"
    if (UpTo(4).drop(9).isNotEmpty()) return \"fail past the end\"
    val refused = try {{ UpTo(4).drop(-1); \"none\" }} catch (e: IllegalArgumentException) {{ \"raised\" }}
    if (refused != \"raised\") return \"fail negative \" + refused
    return \"OK\"
}}
"
    );
    every_backend_agrees_with_kotlinc("native_walk_drop", &source);
}

/// `reduce` is `fold` starting from the first element, so an empty walk has no answer at all — and
/// the exception for that is the UNSUPPORTED one, not the element accessors'.
#[test]
fn a_walk_reduces_from_its_first_element() {
    let source = format!(
        "{COUNTING_UP}
fun box(): String {{
    if (UpTo(4).reduce {{ a, b -> a + b }} != 10) return \"fail sum\"
    if (UpTo(1).reduce {{ a, b -> a + b }} != 1) return \"fail one\"
    if (listOf(2, 3, 4).reduce {{ a, b -> a * b }} != 24) return \"fail list\"
    val refused = try {{
        UpTo(0).reduce {{ a, b -> a + b }}; \"none\"
    }} catch (e: UnsupportedOperationException) {{ \"raised\" }}
    if (refused != \"raised\") return \"fail empty \" + refused
    return \"OK\"
}}
"
    );
    every_backend_agrees_with_kotlinc("native_walk_reduce", &source);
}

/// `lastIndexOf` runs the WHOLE walk, unlike `indexOf` which stops at the first match.
#[test]
fn a_walk_answers_the_last_index_of_a_value() {
    let source = r#"
class Repeating : Iterable<Int> {
    inner class Walk : Iterator<Int> {
        private var at = 0
        override fun hasNext() = at < 5
        override fun next(): Int { at++; return at % 2 }
    }
    override fun iterator(): Iterator<Int> = Walk()
}

fun box(): String {
    val xs = Repeating()
    if (xs.lastIndexOf(1) != 4) return "fail last one"
    if (xs.lastIndexOf(0) != 3) return "fail last zero"
    if (xs.lastIndexOf(9) != -1) return "fail miss"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_walk_last_index_of", source);
}

/// The LIST keeps `lastIndexOf`, answered from its array rather than by walking.
#[test]
fn a_list_answers_the_last_index_of_a_value() {
    let source = r#"
fun box(): String {
    if (listOf(1, 2, 1).lastIndexOf(1) != 2) return "fail list"
    if (listOf(1, 2, 1).lastIndexOf(9) != -1) return "fail miss"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_list_last_index_of", source);
}

/// `sorted`/`sortedBy` order by the elements or by what a selector answers; neither touches the
/// receiver, and both are STABLE — equal elements keep the order the walk gave them.
#[test]
fn a_walk_sorts_by_its_elements_or_by_a_selector() {
    let source = r#"
fun box(): String {
    val xs = listOf(3, 1, 2)
    if (xs.sorted() != listOf(1, 2, 3)) return "fail sorted"
    if (xs != listOf(3, 1, 2)) return "fail receiver changed"
    if (listOf("bb", "a", "ccc").sortedBy { it.length } != listOf("a", "bb", "ccc")) return "fail by"
    if (listOf<Int>().sorted().isNotEmpty()) return "fail empty"
    val pairs = listOf("bx", "ay", "az", "bw")
    if (pairs.sortedBy { it.first() } != listOf("ay", "az", "bx", "bw")) return "fail stable"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_walk_sorted", source);
}

/// `sort()` is the LIST's own: it reorders the receiver and answers nothing.
#[test]
fn a_mutable_list_sorts_itself_in_place() {
    let source = r#"
fun box(): String {
    val xs = mutableListOf(3, 1, 2)
    xs.sort()
    if (xs != listOf(1, 2, 3)) return "fail " + xs
    val empty = mutableListOf<Int>()
    empty.sort()
    if (empty.isNotEmpty()) return "fail empty"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_list_sort", source);
}

/// The extremes of a walk, by the elements and by a selector. An empty walk answers null, and the
/// FIRST of several equal winners is the one that comes back.
#[test]
fn a_walk_answers_its_smallest_and_largest_element() {
    let source = r#"
fun box(): String {
    val xs = listOf(3, 1, 2)
    if (xs.minOrNull() != 1) return "fail min"
    if (xs.maxOrNull() != 3) return "fail max"
    if (listOf<Int>().minOrNull() != null) return "fail empty min"
    if (listOf<Int>().maxOrNull() != null) return "fail empty max"
    val words = listOf("bb", "a", "ccc")
    if (words.minByOrNull { it.length } != "a") return "fail min by"
    if (words.maxByOrNull { it.length } != "ccc") return "fail max by"
    val ties = listOf("ax", "ay", "az")
    if (ties.minByOrNull { it.length } != "ax") return "fail first of equal minima"
    if (ties.maxByOrNull { it.length } != "ax") return "fail first of equal maxima"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_walk_min_max", source);
}

/// `sum` is `sumOf` over the elements themselves, at the width the elements are.
#[test]
fn a_walk_sums_its_elements() {
    let source = r#"
fun box(): String {
    if (listOf(1, 2, 3).sum() != 6) return "fail int"
    if (listOf<Int>().sum() != 0) return "fail empty"
    if (listOf(1L, 2L).sum() != 3L) return "fail long"
    if ((1..4).sum() != 10) return "fail range"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_walk_sum", source);
}

/// `val (a, b) = list`: one member per position, up to five.
#[test]
fn a_list_destructures_by_position() {
    let source = r#"
fun box(): String {
    val (a, b, c) = listOf(1, 2, 3)
    if (a != 1 || b != 2 || c != 3) return "fail three"
    val (p, q, r, s, t) = listOf("a", "b", "c", "d", "e")
    if (p + q + r + s + t != "abcde") return "fail five"
    val pairs = listOf(1 to "x", 2 to "y")
    var joined = ""
    for ((n, name) in pairs) joined += "" + n + name
    if (joined != "1x2y") return "fail pairs " + joined
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_list_components", source);
}

/// `getOrElse` hands the INDEX to the lambda, not the list and not nothing.
#[test]
fn a_list_falls_back_for_an_index_it_does_not_hold() {
    let source = r#"
fun box(): String {
    val xs = listOf(10, 20)
    if (xs.getOrElse(1) { -1 } != 20) return "fail present"
    if (xs.getOrElse(5) { it * 100 } != 500) return "fail index given"
    if (xs.getOrElse(-1) { 7 } != 7) return "fail negative"
    if (listOf<Int>().getOrElse(0) { 9 } != 9) return "fail empty"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_list_get_or_else", source);
}

/// The ends taken OFF a list, or null where `removeFirst`/`removeLast` would raise.
#[test]
fn a_mutable_list_gives_up_its_ends_or_nothing() {
    let source = r#"
fun box(): String {
    val xs = mutableListOf(1, 2, 3)
    if (xs.removeLastOrNull() != 3) return "fail last"
    if (xs.removeFirstOrNull() != 1) return "fail first"
    if (xs != listOf(2)) return "fail remaining " + xs
    if (xs.removeLastOrNull() != 2) return "fail only"
    if (xs.removeLastOrNull() != null) return "fail empty last"
    if (xs.removeFirstOrNull() != null) return "fail empty first"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_list_remove_ends", source);
}

/// `zip` stops at the shorter walk; `toMutableSet` collapses duplicates.
#[test]
fn a_walk_zips_with_another_and_collects_into_a_set() {
    let source = r#"
fun box(): String {
    val zipped = listOf(1, 2, 3).zip(listOf("a", "b"))
    if (zipped != listOf(1 to "a", 2 to "b")) return "fail zip " + zipped
    if (listOf<Int>().zip(listOf("a")).isNotEmpty()) return "fail zip empty"
    val set = listOf(1, 2, 2, 3, 1).toMutableSet()
    if (set.size != 3) return "fail set size " + set.size
    if (!set.contains(2) || set.contains(9)) return "fail set members"
    set.add(9)
    if (!set.contains(9)) return "fail set writable"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_walk_zip_and_set", source);
}

/// A receiver typed `Collection` proves nothing about its LAYOUT.
///
/// `Collection` is admitted as a list type, because the list entry points answer every question a
/// list-shaped collection is asked. But a `Set` is a collection too and is not shaped like a list,
/// so `kt_list_size` read a count out of fields the object does not have — silently, as a garbage
/// number rather than a decline. `setOf(1, 2, 3).size` was right; the same set behind a
/// `Collection<*>` was not. The runtime now asks the OBJECT and counts the walk for anything that
/// is not one of its two list shapes.
///
/// The corpus case is `codegen/box/collectionLiterals/stdlibCollections.kt`, which reaches it
/// through `Collection<*>.checkEquals(vararg es: Any?)`.
#[test]
fn a_set_behind_a_collection_still_counts_itself() {
    let source = r#"
fun Collection<*>.viaCollection(): Int = size
fun Collection<*>.emptyViaCollection(): Boolean = isEmpty()

fun box(): String {
    val s = setOf(1, 2, 3)
    if (s.size != 3) return "fail direct " + s.size
    if (s.viaCollection() != 3) return "fail set via Collection " + s.viaCollection()
    if (s.emptyViaCollection()) return "fail set empty"
    if (setOf<Int>().viaCollection() != 0) return "fail empty set"
    if (!setOf<Int>().emptyViaCollection()) return "fail empty set is empty"
    val l = listOf(1, 2)
    if (l.viaCollection() != 2) return "fail list via Collection"
    if (mutableListOf(1, 2, 3).viaCollection() != 3) return "fail mutable list"
    val m = mutableSetOf(1, 2)
    m.add(3)
    if (m.viaCollection() != 3) return "fail mutable set " + m.viaCollection()
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_collection_size", source);
}

/// `xs + x` and `xs + ys` over an iterable the runtime cannot INDEX — a progression, say.
///
/// Which of the two a call means is the DECLARATION's answer, read from its physical parameter:
/// after substitution an element of type `List<T>` and a collection of them look exactly alike,
/// and the argument's own type cannot tell them apart.
///
/// Not asserted here, because it is the FRONTEND's and fails on both of krusty's backends:
/// `listOf(listOf(1)) + listOf(2)` selects the collection overload and answers `[[1], 2]` where
/// Kotlin selects the element one and answers `[[1], [2]]`. Confirmed pre-existing by removing
/// every `src/native/` change and re-running.
#[test]
fn a_walk_adds_an_element_or_another_walk() {
    let source = r#"
fun box(): String {
    val range = 0..3
    if (range + 4 != listOf(0, 1, 2, 3, 4)) return "fail element " + (range + 4)
    if (range + listOf(9) != listOf(0, 1, 2, 3, 9)) return "fail collection"
    if (range + emptyList<Int>() != listOf(0, 1, 2, 3)) return "fail empty collection"
    if (listOf(1) + 2 != listOf(1, 2)) return "fail list element"
    if (listOf(1) + listOf(2) != listOf(1, 2)) return "fail list collection"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_walk_plus", source);
}

/// A TERMINAL operation over a sequence consumes every element, so an eager walk is Kotlin's own.
///
/// What makes an eager `map` wrong over a sequence is the ANSWER it hands on — the transform would
/// run for elements a later `first()` never asks for — and a terminal operation has no such answer.
#[test]
fn a_sequence_answers_its_terminal_members() {
    let source = r#"
fun box(): String {
    val seq = listOf(1, 2, 3).asSequence()
    var sum = 0
    seq.forEach { sum += it }
    if (sum != 6) return "fail forEach " + sum
    if (seq.joinToString() != "1, 2, 3") return "fail joinToString " + seq.joinToString()
    if (sequenceOf("a", "b").joinToString() != "a, b") return "fail sequenceOf"
    if (sequenceOf<Int>().joinToString() != "") return "fail empty sequenceOf"
    if (emptySequence<Int>().joinToString() != "") return "fail emptySequence"
    var single = 0
    sequenceOf(7).forEach { single = it }
    if (single != 7) return "fail single"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_sequence_terminal", source);
}
