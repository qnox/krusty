//! A class that overrides a member of a type declared OUTSIDE the file.
//!
//! The class model refused every such class by name. It need not: the override takes a slot of its
//! own, like any member the class declares freshly — a caller naming the CLASS reaches it, and a
//! caller naming the dependency type declines at the call site, where the type it named is still
//! in sight. What the refusal was protecting is narrower than a whole class, and is kept:
//!
//! - `invoke` on a function type, whose slot number is FIXED (the runtime names it), so an
//!   override that cannot take that slot would be reached there anyway and still declines.
//! - The runtime's own answers for a dependency member. A receiver typed by a runtime-known type
//!   this file implements may be an object of the PROGRAM's, and those tables answer only for the
//!   ones the runtime makes — no static type tells the two apart, which is why the answer is the
//!   runtime's at all. So a member asked of such a type declines by name.
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

/// A call through the DEPENDENCY type now WORKS, where it once declined.
///
/// This test read the other way when it was written, and the header above still describes why: the
/// answer for such a member is the runtime's, and the runtime answers only for the objects it makes.
/// What changed is that a ZERO-ARGUMENT member no longer needs the runtime to answer it — the file
/// knows every class of its own that could stand behind the type, so the call site tests the
/// receiver and dispatches on the implementor's own slot. See
/// `tests/native_implemented_dependency_dispatch_e2e.rs` for the mechanism and its limits.
///
/// A member with ARGUMENTS still declines, which the last test in this file pins.
#[test]
fn a_call_through_the_dependency_type_dispatches_on_the_receiver() {
    let source = "class Pointed : Iterator<String> {\n\
         \x20   var left = 1\n\
         \x20   override fun hasNext(): Boolean = left > 0\n\
         \x20   override fun next(): String { left--; return \"OK\" }\n\
         }\n\
         fun walk(it: Iterator<String>): String = if (it.hasNext()) it.next() else \"fail\"\n\
         fun box(): String = walk(Pointed())\n";
    expect_box_ok_with_stdlib(source, "DependencyTypedCall");
    expect_native_box(source, "DependencyTypedCall", "OK");
}

/// A member asked of a type this file implements ITSELF declines, whatever the member.
///
/// `class Chars : CharsBase(s), CharSequence` and then `value: CharSequence` — the receiver may be
/// a `Chars` or a string, and the table that answers `CharSequence.get` answers only for a string.
#[test]
fn a_member_of_a_type_this_file_implements_declines() {
    expect_native_decline(
        "open class CharsBase(protected val s: String) {\n\
         \x20   val length: Int get() = s.length\n\
         \x20   operator fun get(index: Int): Char = s[index]\n\
         \x20   fun subSequence(startIndex: Int, endIndex: Int): CharSequence =\n\
         \x20       s.subSequence(startIndex, endIndex)\n\
         }\n\
         class Chars(s: String) : CharsBase(s), CharSequence\n\
         fun box(): String {\n\
         \x20   val value: CharSequence = Chars(\"OK\")\n\
         \x20   return \"\" + value[0] + value[1]\n\
         }\n",
        "ImplementedDependencyMember",
        "this file implements itself",
    );
}
