//! `List.subList` on a list the runtime made.
//!
//! The range is a view of its list, not a copy: a write a growable parent makes with `set` shows
//! through it, and a change of the parent's shape ends the view with a
//! `ConcurrentModificationException`. A range that writes through (`MutableList.subList`) is another
//! member, which the runtime does not realize, so it declines by name instead of running.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{
    expect_box_ok_with_stdlib, expect_native_box, expect_native_decline, kotlinc_box_result,
};

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

/// A range of a growable list, read through the `List` face, reads the list as it is now.
#[test]
fn a_range_of_a_growable_list_reads_the_list_it_was_made_from() {
    let source = "fun box(): String {\n\
         \x20   val growable = mutableListOf(1, 2, 3, 4)\n\
         \x20   val readable: List<Int> = growable\n\
         \x20   val range = readable.subList(1, 3)\n\
         \x20   if (range.size != 2 || range[0] != 2 || range[1] != 3) return \"fail initial $range\"\n\
         \x20   growable[1] = 20\n\
         \x20   if (range[0] != 20) return \"fail write through ${range[0]}\"\n\
         \x20   if (range != listOf(20, 3)) return \"fail equals $range\"\n\
         \x20   growable.add(5)\n\
         \x20   try {\n\
         \x20       range.size\n\
         \x20       return \"fail stale size\"\n\
         \x20   } catch (e: ConcurrentModificationException) {\n\
         \x20   }\n\
         \x20   try {\n\
         \x20       range[0]\n\
         \x20       return \"fail stale get\"\n\
         \x20   } catch (e: ConcurrentModificationException) {\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("GrowableListRange", source);
}

/// A range that writes through is not realized: the program is declined, not run.
#[test]
fn a_range_that_writes_through_declines_by_name() {
    let source = "fun box(): String {\n\
         \x20   val growable: MutableList<Int> = mutableListOf(1, 2, 3)\n\
         \x20   growable.subList(0, 2).clear()\n\
         \x20   return if (growable == listOf(3)) \"OK\" else \"fail\"\n\
         }\n";
    expect_native_decline(
        source,
        "WritingRange",
        "the member `java.util.List.subList`",
    );
}

/// A change of the list at the root ends every walk and every range taken from it, nested ones
/// too.
#[test]
fn a_change_of_the_root_list_invalidates_every_range_and_walk() {
    let source = r#"fun box(): String {
    val growable = mutableListOf(1, 2, 3, 4)
    val readable: List<Int> = growable
    val outer = readable.subList(0, 3)
    val inner = outer.subList(1, 2)
    val walk = outer.iterator()
    growable.add(5)
    try {
        walk.next()
        return "walk"
    } catch (e: ConcurrentModificationException) {
    }
    try {
        outer.size
        return "outer"
    } catch (e: ConcurrentModificationException) {
    }
    try {
        inner.size
        return "inner size"
    } catch (e: ConcurrentModificationException) {
    }
    try {
        inner[0]
        return "inner get"
    } catch (e: ConcurrentModificationException) {
    }
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("RootChange", source);
}

/// Appending a range of the list to the list itself reads the range before the first append.
#[test]
fn a_list_takes_a_range_of_itself_before_it_appends() {
    let source = r#"fun box(): String {
    val root = ArrayList<Int>()
    root.add(1)
    root.add(2)
    root.add(3)
    val view = (root as List<Int>).subList(0, 2)
    root.addAll(view)
    if (root != listOf(1, 2, 3, 1, 2)) return "aliased"
    val self = ArrayList<Int>()
    self.add(7)
    self.addAll(self)
    if (self != listOf(7, 7)) return "self"
    val other = ArrayList<Int>()
    other.addAll(listOf(4, 5).subList(0, 2))
    if (other != listOf(4, 5)) return "disjoint"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("AddAllOfView", source);
}
