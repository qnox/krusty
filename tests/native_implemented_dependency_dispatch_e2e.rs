//! A zero-argument member asked of a runtime-known type this file implements ITSELF.
//!
//! The runtime answers such a member for the objects IT makes, and a class of the program's is not
//! one of them — no static type tells the two apart, which is why the answer is the runtime's at
//! all, and why this was a decline. But where the member takes NO ARGUMENTS the choice can be made
//! at the call site: the file knows every class of its own that could stand behind that type, so
//! the receiver is tested against each, dispatched on that implementor's own slot when it matches,
//! and handed to the runtime entry point otherwise.
//!
//! No program-wide slot number is needed for this, which is what the implementation plan first
//! assumed. The decline is raised precisely when the implementor is in THIS file, so its slot is
//! one this file already assigned. A subclass needs no entry of its own either: `is` walks the
//! super chain, and a subclass's vtable has already replaced the slot the dispatch reads.
//!
//! Zero arguments on purpose. An argument would have to cross at the DECLARATION's carriers in one
//! arm and at the runtime entry point's in the other; `iterator`, `hasNext` and `next` take none.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, expect_native_decline};

/// A program's own `Iterator`, reached through the type it implements.
#[test]
fn a_program_iterator_is_walked_through_the_type_it_implements() {
    let source = "class Pointed : Iterator<String> {\n\
         \x20   var left = 2\n\
         \x20   override fun hasNext(): Boolean = left > 0\n\
         \x20   override fun next(): String {\n\
         \x20       left--\n\
         \x20       return if (left == 1) \"O\" else \"K\"\n\
         \x20   }\n\
         }\n\
         fun walk(source: Iterator<String>): String {\n\
         \x20   var text = \"\"\n\
         \x20   while (source.hasNext()) text += source.next()\n\
         \x20   return text\n\
         }\n\
         fun box(): String = walk(Pointed())\n";
    expect_box_ok_with_stdlib(source, "ProgramIterator");
    expect_native_box(source, "ProgramIterator", "OK");
}

/// The runtime arm is emitted and is not yet reachable, which is worth stating rather than
/// pretending otherwise.
///
/// A file that declares a collection of its own still declines every collection member asked of a
/// CONCRETE runtime type — `listOf(…).iterator()` among them — through a blanket file-level guard
/// that predates this dispatch. So inside such a file there is no way to obtain a runtime iterator,
/// and the fall-through arm cannot be exercised from Kotlin source today. It is still what the
/// generator must emit: the arm becomes live the moment that guard is made receiver-precise, and
/// emitting a dispatch whose last arm was a trap would be the wrong shape to leave behind.
///
/// An ANONYMOUS object is an implementor like any other, and a SUBCLASS needs no entry of its own.
#[test]
fn an_anonymous_implementor_and_a_subclass_both_dispatch() {
    let source = "open class Counting(var left: Int) : Iterator<String> {\n\
         \x20   override fun hasNext(): Boolean = left > 0\n\
         \x20   override fun next(): String { left--; return \"a\" }\n\
         }\n\
         class Louder(left: Int) : Counting(left) {\n\
         \x20   override fun next(): String { left--; return \"A\" }\n\
         }\n\
         fun walk(source: Iterator<String>): String {\n\
         \x20   var text = \"\"\n\
         \x20   while (source.hasNext()) text += source.next()\n\
         \x20   return text\n\
         }\n\
         fun box(): String {\n\
         \x20   if (walk(Counting(2)) != \"aa\") return \"fail base\"\n\
         \x20   // The slot is the base's; the subclass's vtable has replaced it.\n\
         \x20   if (walk(Louder(2)) != \"AA\") return \"fail subclass\"\n\
         \x20   val anonymous = object : Iterator<String> {\n\
         \x20       var left = 2\n\
         \x20       override fun hasNext(): Boolean = left > 0\n\
         \x20       override fun next(): String { left--; return \"z\" }\n\
         \x20   }\n\
         \x20   if (walk(anonymous) != \"zz\") return \"fail anonymous\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "SubclassImplementor");
    expect_native_box(source, "SubclassImplementor", "OK");
}

/// A member that takes ARGUMENTS dispatches the same way, on the same receiver test.
///
/// An argument adds one thing and only one: the two sides state the operand differently — the
/// implementor's own parameter type in its arm, the runtime entry point's carrier in the last —
/// so each is converted per arm from a value evaluated ONCE, before the tests. Evaluating per arm
/// would run a side effect twice, which is what the operand below is there to catch.
#[test]
fn a_member_with_arguments_dispatches_on_the_receiver_too() {
    let source = "class Chars(val text: String) : CharSequence {\n\
         \x20   override val length: Int get() = text.length\n\
         \x20   override fun get(index: Int): Char = text[index]\n\
         \x20   override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =\n\
         \x20       Chars(text.substring(startIndex, endIndex))\n\
         }\n\
         var reads = 0\n\
         fun counted(): Int {\n\
         \x20   reads++\n\
         \x20   return 0\n\
         }\n\
         fun first(value: CharSequence): Char = value[counted()]\n\
         fun box(): String {\n\
         \x20   // A class of this file's behind the type, and a string the runtime made, at one\n\
         \x20   // call site.\n\
         \x20   if (first(Chars(\"OK\")) != 'O') return \"fail mine\"\n\
         \x20   if (first(\"OK\") != 'O') return \"fail the runtime's\"\n\
         \x20   return if (reads == 2) \"OK\" else \"fail the index ran \" + reads + \" times\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ArgumentedImplementedMember");
    expect_native_box(source, "ArgumentedImplementedMember", "OK");
}

/// A member Kotlin gives a SPECIAL BRIDGE declines, argument or no argument.
///
/// `Map<Any, Any>.get(key: Any)` is declared with a NON-NULL parameter, and a caller holding the
/// same object as a `Map<Any?, Any?>` may pass `null`. Kotlin does not call the override there — it
/// answers the member's default, `null` for `get`, because the argument cannot be what the
/// declaration accepts. The receiver dispatch has no bridge to put in front of an implementor's
/// arm, so it declines rather than calling an override Kotlin would have skipped.
///
/// The two-sided conformance gate is what found this: dispatching these turned four corpus cases
/// from declines into wrong answers, one of them a SIGILL.
#[test]
fn a_member_with_a_special_bridge_declines() {
    expect_native_decline(
        "object NotEmptyMap : MutableMap<Any, Any> {\n\
         \x20   override fun containsKey(key: Any): Boolean = true\n\
         \x20   override fun containsValue(value: Any): Boolean = true\n\
         \x20   override fun get(key: Any): Any? = Any()\n\
         \x20   override fun remove(key: Any): Any? = Any()\n\
         \x20   override val size: Int get() = 0\n\
         \x20   override fun isEmpty(): Boolean = true\n\
         \x20   override fun put(key: Any, value: Any): Any? = null\n\
         \x20   override fun putAll(from: Map<out Any, Any>) {}\n\
         \x20   override fun clear() {}\n\
         \x20   override val entries: MutableSet<MutableMap.MutableEntry<Any, Any>> get() = null!!\n\
         \x20   override val keys: MutableSet<Any> get() = null!!\n\
         \x20   override val values: MutableCollection<Any> get() = null!!\n\
         }\n\
         fun box(): String {\n\
         \x20   val n = NotEmptyMap as MutableMap<Any?, Any?>\n\
         \x20   return if (n.get(null) == null) \"OK\" else \"fail\"\n\
         }\n",
        "SpecialBridgeMember",
        "get",
    );
}
