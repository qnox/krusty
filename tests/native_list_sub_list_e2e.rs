//! `List.subList` on a list the runtime made.
//!
//! The range is a view of its list, not a copy: a write a growable parent makes with `set` shows
//! through it, and a change of the parent's shape ends the view with a
//! `ConcurrentModificationException`. A range of a growable list writes through to it.
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

/// A range of a growable list, read through the `List` face, reads the list as it is now.
#[test]
fn a_range_of_a_growable_list_reads_the_list_it_was_made_from() {
    every_backend_agrees_with_kotlinc(
        "GrowableListRange",
        include_str!("box_regressions/listSubListGrowableView.kt"),
    );
}

/// A range of a growable list writes through to the list, and the list's shape follows.
#[test]
fn a_range_that_writes_through_changes_the_list() {
    let source = "fun box(): String {\n\
         \x20   val growable = mutableListOf(1, 2, 3, 4, 5)\n\
         \x20   val range = growable.subList(1, 4)\n\
         \x20   range[0] = 20\n\
         \x20   if (growable != listOf(1, 20, 3, 4, 5)) return \"fail set $growable\"\n\
         \x20   range.add(9)\n\
         \x20   if (growable != listOf(1, 20, 3, 4, 9, 5) || range.size != 4) return \"fail add $growable $range\"\n\
         \x20   range.add(1, 7)\n\
         \x20   if (growable != listOf(1, 20, 7, 3, 4, 9, 5)) return \"fail add at $growable\"\n\
         \x20   if (range.removeAt(0) != 20 || range.remove(9) != true) return \"fail remove\"\n\
         \x20   if (growable != listOf(1, 7, 3, 4, 5) || range != listOf(7, 3, 4)) return \"fail removed $growable $range\"\n\
         \x20   range.clear()\n\
         \x20   if (growable != listOf(1, 5) || range.size != 0) return \"fail clear $growable\"\n\
         \x20   val inner = growable.subList(0, 2).subList(1, 2)\n\
         \x20   inner.add(8)\n\
         \x20   if (growable != listOf(1, 5, 8)) return \"fail nested $growable\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("WritingRange", source);
}

/// A range of a read-only list refuses a write, as the list does.
#[test]
fn a_range_of_a_read_only_list_refuses_a_write() {
    let source = "fun box(): String {\n\
         \x20   val range = (listOf(1, 2, 3) as MutableList<Int>).subList(0, 2)\n\
         \x20   try {\n\
         \x20       range.add(4)\n\
         \x20       return \"fail\"\n\
         \x20   } catch (e: UnsupportedOperationException) {\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ReadOnlyRange", source);
}

/// A walk of a range, and a range of a range, see a write made through the range as a change of
/// shape, as a growable list's own do.
#[test]
fn a_write_through_a_range_invalidates_its_walks_and_its_ranges() {
    let source = "fun box(): String {\n\
         \x20   val growable = mutableListOf(1, 2, 3, 4, 5)\n\
         \x20   val outer = growable.subList(0, 4)\n\
         \x20   val walk = outer.iterator()\n\
         \x20   walk.next()\n\
         \x20   outer.add(0, 9)\n\
         \x20   try {\n\
         \x20       walk.next()\n\
         \x20       return \"fail walk\"\n\
         \x20   } catch (e: ConcurrentModificationException) {\n\
         \x20   }\n\
         \x20   val inner = outer.subList(1, 3)\n\
         \x20   outer.add(0, 8)\n\
         \x20   try {\n\
         \x20       inner[0]\n\
         \x20       return \"fail inner\"\n\
         \x20   } catch (e: ConcurrentModificationException) {\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("InvalidatedByWrite", source);
}

/// An empty range still answers to its list: clearing one after the list changed raises, as the
/// JVM's `AbstractList.clear` does by reading the range's size.
#[test]
fn clearing_an_empty_range_of_a_changed_list_raises() {
    let source = r#"fun box(): String {
    val root = mutableListOf(1)
    val empty = root.subList(0, 0)
    root.add(2)
    try {
        empty.clear()
        return "missing concurrent modification"
    } catch (e: ConcurrentModificationException) {
        return "OK"
    }
}
"#;
    every_backend_agrees_with_kotlinc("EmptyRangeClear", source);
}

/// A change of the list at the root ends every walk and every range taken from it, nested ones
/// too.
#[test]
fn a_change_of_the_root_list_invalidates_every_range_and_walk() {
    every_backend_agrees_with_kotlinc(
        "RootChange",
        include_str!("box_regressions/listSubListRootChange.kt"),
    );
}

/// Appending a range of the list to the list itself reads the range before the first append.
#[test]
fn a_list_takes_a_range_of_itself_before_it_appends() {
    every_backend_agrees_with_kotlinc(
        "AddAllOfView",
        include_str!("box_regressions/listSubListAddAllOfView.kt"),
    );
}
