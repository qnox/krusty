//! A class that overrides a member of a type declared OUTSIDE the file.
//!
//! The class model refused every such class by name. It need not: the override takes a slot of its
//! own, like any member the class declares freshly — a caller naming the CLASS reaches it, and a
//! caller naming the dependency type declines at the call site, where the type it named is still
//! in sight. What the refusal was protecting is narrower than a whole class, and is kept:
//!
//! - `invoke` on a function type, whose slot number is FIXED (the runtime names it), so an
//!   override that cannot take that slot would be reached there anyway and still declines.
//! - The runtime's COLLECTION dispatch. A receiver typed `List`, `Iterable` or `Iterator` goes to
//!   a dispatch that knows only the collections this runtime makes, and no static type tells the
//!   two apart — so a file that declares one of its own declines those members by name.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, expect_native_decline};

/// A dependency METHOD overridden, reached through the class that declares it.
#[test]
fn a_class_overriding_a_dependency_method_is_called_through_its_own_type() {
    let source = "class Counted(val text: String) : CharSequence {\n\
         \x20   var reads = 0\n\
         \x20   override val length: Int get() = text.length\n\
         \x20   override fun get(index: Int): Char {\n\
         \x20       reads++\n\
         \x20       return text[index]\n\
         \x20   }\n\
         \x20   override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =\n\
         \x20       Counted(text.substring(startIndex, endIndex))\n\
         }\n\
         fun box(): String {\n\
         \x20   val counted = Counted(\"OK!\")\n\
         \x20   if (counted.length != 3) return \"fail length\"\n\
         \x20   if (counted[0] != 'O') return \"fail get\"\n\
         \x20   if (counted.reads != 1) return \"fail the count\"\n\
         \x20   return \"\" + counted[0] + counted[1]\n\
         }\n";
    expect_box_ok_with_stdlib(source, "DependencyMethodOverride");
    expect_native_box(source, "DependencyMethodOverride", "OK");
}

/// A dependency PROPERTY overridden, which had its own refusal beside the method one.
#[test]
fn a_class_overriding_a_dependency_property_reads_it_through_its_own_type() {
    let source = "class Box(val value: String) : Comparable<Box> {\n\
         \x20   override fun compareTo(other: Box): Int = value.compareTo(other.value)\n\
         }\n\
         class Pointed : Iterator<String> {\n\
         \x20   var left = 2\n\
         \x20   override fun hasNext(): Boolean = left > 0\n\
         \x20   override fun next(): String {\n\
         \x20       left--\n\
         \x20       return if (left == 1) \"O\" else \"K\"\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   if (Box(\"a\").compareTo(Box(\"b\")) >= 0) return \"fail compareTo\"\n\
         \x20   if (Box(\"b\").compareTo(Box(\"a\")) <= 0) return \"fail the other way\"\n\
         \x20   val walk = Pointed()\n\
         \x20   var text = \"\"\n\
         \x20   while (walk.hasNext()) text += walk.next()\n\
         \x20   return text\n\
         }\n";
    expect_box_ok_with_stdlib(source, "DependencyPropertyOverride");
    expect_native_box(source, "DependencyPropertyOverride", "OK");
}

/// A call through the DEPENDENCY type still declines, where the type it named is in sight.
#[test]
fn a_call_through_the_dependency_type_still_declines() {
    expect_native_decline(
        "class Pointed : Iterator<String> {\n\
         \x20   var left = 1\n\
         \x20   override fun hasNext(): Boolean = left > 0\n\
         \x20   override fun next(): String { left--; return \"OK\" }\n\
         }\n\
         fun walk(it: Iterator<String>): String = if (it.hasNext()) it.next() else \"fail\"\n\
         fun box(): String = walk(Pointed())\n",
        "DependencyTypedCall",
        "Iterator.hasNext",
    );
}
