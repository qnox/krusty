//! `x is List<*>` — an `is` check against `kotlin.collections.List`.
//!
//! The runtime builds two kinds of list: the immutable one `listOf` answers and the growable one
//! `ArrayList()` answers. A check writes the INTERFACE, which is neither of those types, so both
//! name a marker and the check compares against that.
//!
//! A file that declares a list of its OWN keeps declining. Such a class wears no marker, and
//! answering `false` for an object that is a list would be a wrong answer rather than a refusal.
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

/// The immutable list `listOf` answers.
#[test]
fn a_built_list_is_a_list() {
    let source = "fun box(): String {\n\
         \x20   val x: Any = listOf(1, 2, 3)\n\
         \x20   return if (x is List<*>) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("BuiltListIsList", source);
}

/// The GROWABLE one, which is a different runtime type wearing the same marker.
#[test]
fn a_growable_list_is_a_list_too() {
    let source = "fun box(): String {\n\
         \x20   val x: Any = ArrayList<Int>()\n\
         \x20   return if (x is List<*>) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("GrowableListIsList", source);
}

/// A value that is no list answers false, so the marker is not on everything.
#[test]
fn a_value_that_is_no_list_answers_false() {
    let source = "fun box(): String {\n\
         \x20   val s: Any = \"text\"\n\
         \x20   val n: Any = 1\n\
         \x20   return if (s is List<*> || n is List<*>) \"fail\" else \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("NonListIsNotList", source);
}

/// Narrowed by the check, the value is usable as a list — which is what the corpus case does.
#[test]
fn a_checked_list_is_walkable_after_the_check() {
    let source = "fun total(x: Any): Int {\n\
         \x20   var sum = 0\n\
         \x20   if (x is List<*>) {\n\
         \x20       for (i in x.indices) sum = sum * 10 + i\n\
         \x20   }\n\
         \x20   return sum\n\
         }\n\
         fun box(): String {\n\
         \x20   val answer = total(listOf(0, 0, 0, 0))\n\
         \x20   return if (answer == 123) \"OK\" else \"fail \" + answer\n\
         }\n";
    every_backend_agrees_with_kotlinc("CheckedListWalks", source);
}

/// Through a TYPEALIAS, which is the other corpus case: the alias is the same type.
#[test]
fn a_list_answers_through_an_alias_of_it() {
    let source = "typealias L<T> = List<T>\n\
         fun box(): String {\n\
         \x20   val test: Collection<Int> = listOf(1, 2, 3)\n\
         \x20   if (test !is L) return \"fail is\"\n\
         \x20   val again = test as L\n\
         \x20   return if (again.size == 3) \"OK\" else \"fail size\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ListThroughAlias", source);
}

/// A CAST reads the same answer the check does, and fails where the check would answer false.
#[test]
fn a_cast_to_list_fails_on_a_value_that_is_none() {
    let source = "fun box(): String {\n\
         \x20   val s: Any = \"text\"\n\
         \x20   return try {\n\
         \x20       s as List<*>\n\
         \x20       \"fail no throw\"\n\
         \x20   } catch (e: ClassCastException) {\n\
         \x20       \"OK\"\n\
         \x20   }\n\
         }\n";
    every_backend_agrees_with_kotlinc("CastToListFails", source);
}

/// A file declaring a collection of its OWN keeps declining: its class wears no marker, and
/// `false` would be a wrong answer for an object that the program can make a list of.
///
/// An `Iterable` rather than a full `List`, because the guard asks about the SHAPE a file
/// implements rather than about the exact interface, and a `List` of one's own declines earlier
/// for members this generator does not realize — which would test a different rule.
#[test]
fn a_file_that_declares_its_own_collection_declines_the_check() {
    let source = "class Own : Iterable<Int> {\n\
         \x20   override fun iterator(): Iterator<Int> = listOf(1).iterator()\n\
         }\n\
         fun box(): String {\n\
         \x20   val x: Any = Own()\n\
         \x20   return if (x is List<*>) \"fail\" else \"OK\"\n\
         }\n";
    expect_native_decline(source, "OwnCollectionDeclines", "is` check against");
}
