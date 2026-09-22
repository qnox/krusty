//! Which receivers a file's own collection class endangers.
//!
//! A member asked of a runtime-known collection type is answered by the runtime, whose tables read
//! the objects IT makes. A file declaring a class behind one of those types puts an object of the
//! program's there too, and no static type tells the two apart — so such a member declines.
//!
//! What the decline is keyed on is the RECEIVER's shape, not the file. A shape groups the types
//! whose objects are interchangeable at a call site: a set implementor answers a `Collection` and
//! an `Iterable` receiver, so those are one shape; a map is no `Collection` and a sequence is no
//! `Iterable`, so each is its own. Declaring a class of one shape says nothing about a receiver of
//! another, and the guard now says only what the declaration supports.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, expect_native_decline};

/// A file declaring its own ITERATOR still has its lists answered. The iterator it hands out is
/// its own object; a list it builds is the runtime's, and nothing about the former reaches the
/// latter.
#[test]
fn a_declared_iterator_leaves_the_lists_alone() {
    let source = "class Pointed : Iterator<String> {\n\
         \x20   var left = 2\n\
         \x20   override fun hasNext(): Boolean = left > 0\n\
         \x20   override fun next(): String {\n\
         \x20       left--\n\
         \x20       return if (left == 1) \"O\" else \"K\"\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   val xs = listOf(1, 2, 3)\n\
         \x20   if (!xs.contains(2)) return \"fail contains\"\n\
         \x20   if (xs.filter { it > 1 }.size != 2) return \"fail filter\"\n\
         \x20   var total = 0\n\
         \x20   for (x in xs) total += x\n\
         \x20   if (total != 6) return \"fail walk \" + total\n\
         \x20   var text = \"\"\n\
         \x20   val walk = Pointed()\n\
         \x20   while (walk.hasNext()) text += walk.next()\n\
         \x20   return text\n\
         }\n";
    expect_box_ok_with_stdlib(source, "DeclaredIteratorAndLists");
    expect_native_box(source, "DeclaredIteratorAndLists", "OK");
}

/// A file declaring its own map ENTRY still has its lists and its text answered. An entry is
/// neither, and Kotlin's `Map` is no `Collection`, so the shapes do not meet.
#[test]
fn a_declared_map_entry_leaves_the_lists_and_the_text_alone() {
    let source = "class Held(override val key: String, override val value: String) :\n\
         \x20   Map.Entry<String, String>\n\
         fun box(): String {\n\
         \x20   val xs = listOf(1, 2, 3)\n\
         \x20   if (!xs.contains(3)) return \"fail contains\"\n\
         \x20   if (!\"abc\".contains(\"b\")) return \"fail text\"\n\
         \x20   val held = Held(\"O\", \"K\")\n\
         \x20   return held.key + held.value\n\
         }\n";
    expect_box_ok_with_stdlib(source, "DeclaredEntryAndLists");
    expect_native_box(source, "DeclaredEntryAndLists", "OK");
}

/// The coarseness INSIDE a shape is deliberate. Text is walked by the same runtime dispatch a list
/// is, and a `CharSequence` of the program's could stand behind either receiver — so a file that
/// declares one still declines a list member, exactly as it did before the guard grew precise.
#[test]
fn a_declared_char_sequence_still_declines_a_list_member() {
    expect_native_decline(
        "class Chars(private val text: String) : CharSequence {\n\
         \x20   override val length: Int get() = text.length\n\
         \x20   override fun get(index: Int): Char = text[index]\n\
         \x20   override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =\n\
         \x20       Chars(text.substring(startIndex, endIndex))\n\
         }\n\
         fun box(): String {\n\
         \x20   if (Chars(\"OK\").length != 2) return \"fail length\"\n\
         \x20   return if (listOf(1, 2, 3).contains(2)) \"OK\" else \"fail contains\"\n\
         }\n",
        "DeclaredCharsAndLists",
        "contains",
    );
}
