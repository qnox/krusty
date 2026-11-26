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
    every_backend_agrees_with_kotlinc(
        "WritingRange",
        include_str!("box_regressions/listSubListWritingRange.kt"),
    );
}

/// A range of a read-only list refuses a write, as the list does.
///
/// PLATFORM-SPECIFIC: the program casts `listOf(1, 2, 3)` to `MutableList`, which succeeds only
/// where the list is a `java.util.List` (the JVM's `Arrays.asList`) and which the Native runtime
/// answers the same way. A Kotlin/Wasm `listOf` is not a `MutableList`, so the cast throws
/// `ClassCastException` there with kotlinc's own KLIB stdlib too, and no read-only list reaches a
/// `MutableList` view on Wasm to be refused.
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
    every_backend_agrees_with_kotlinc(
        "InvalidatedByWrite",
        include_str!("box_regressions/listSubListInvalidatedByWrite.kt"),
    );
}

/// A walk of a list that changed raises `ConcurrentModificationException` at either end of the
/// list, before it asks whether the walk is at an end, while a valid exhausted walk still raises
/// `NoSuchElementException`.
#[test]
fn a_stale_walk_at_an_end_raises_before_its_end_guard() {
    every_backend_agrees_with_kotlinc(
        "WalkStaleBeforeEnd",
        include_str!("box_regressions/listSubListWalkStaleBeforeEnd.kt"),
    );
}

/// An empty range still answers to its list: clearing one after the list changed raises, as the
/// JVM's `AbstractList.clear` does by reading the range's size.
#[test]
fn clearing_an_empty_range_of_a_changed_list_raises() {
    every_backend_agrees_with_kotlinc(
        "EmptyRangeClear",
        include_str!("box_regressions/listSubListEmptyRangeClear.kt"),
    );
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

/// A list appends itself, and a range of another list, as the elements they hold before the append.
///
/// A list appending a range of itself differs by platform: kotlinc-native's library grows the list
/// before reading the range and raises `ConcurrentModificationException`. That program is the
/// `ArrayListAddAllGrowsBeforeReadingItsArgument` entry of `tests/kotlinc_defects_e2e.rs`, which
/// pins each platform's kotlinc and krusty's fix mode.
#[test]
fn a_list_appends_itself_and_a_range_of_another_list() {
    every_backend_agrees_with_kotlinc(
        "AddAllOfView",
        include_str!("box_regressions/listSubListAddAllOfView.kt"),
    );
}
