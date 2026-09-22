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
