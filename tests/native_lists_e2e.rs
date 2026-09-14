//! `listOf(...)` through krusty's own code generator and runtime.
//!
//! A list is the `Array<T>` a vararg call already built, with a header — which is what Kotlin's own
//! `listOf(vararg)` wraps too, and what makes its elements traced by a collector that already knows
//! how to trace an array. Being read-only is what makes sharing that array sound.
//!
//! The programs here are mostly about ITERATION, because that is what a list is for and because it
//! is where the interesting mistake lives: `iterator` is declared on `Iterable`, which a range also
//! is, so a receiver the generator could only type by that interface may be holding either. The
//! runtime decides from the descriptor, and the pair of loops in
//! `an_interface_typed_receiver_iterates_whichever_it_holds` is that decision's whole point.

use super::common::expect_native_box;

#[test]
fn a_for_loop_walks_a_list_in_order() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   var out = \"\"\n\
         \x20   for (s in listOf(\"O\", \"K\", \"!\")) out += s\n\
         \x20   return if (out == \"OK!\") \"OK\" else \"fail: $out\"\n\
         }\n",
        "ListForLoop",
        "OK",
    );
}

#[test]
fn a_list_answers_its_size_and_its_elements() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val values = listOf(10, 20, 30)\n\
         \x20   if (values.size != 3) return \"fail size: \" + values.size\n\
         \x20   if (values[0] != 10) return \"fail first\"\n\
         \x20   if (values[2] != 30) return \"fail last\"\n\
         \x20   if (values.isEmpty()) return \"fail isEmpty\"\n\
         \x20   if (!values.contains(20)) return \"fail contains\"\n\
         \x20   if (values.contains(40)) return \"fail absent\"\n\
         \x20   if (values.indexOf(20) != 1) return \"fail indexOf\"\n\
         \x20   if (values.indexOf(40) != -1) return \"fail missing\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "ListMembers",
        "OK",
    );
}

#[test]
fn an_empty_list_has_nothing_to_walk() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val empty = listOf<String>()\n\
         \x20   if (empty.size != 0) return \"fail size\"\n\
         \x20   if (!empty.isEmpty()) return \"fail isEmpty\"\n\
         \x20   var count = 0\n\
         \x20   for (s in empty) count += 1\n\
         \x20   return if (count == 0) \"OK\" else \"fail: walked $count\"\n\
         }\n",
        "EmptyList",
        "OK",
    );
}

#[test]
fn a_list_renders_and_compares_by_its_elements() {
    // Kotlin's `List` answers all three of `kotlin.Any`'s members by its contents, not by identity.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val a = listOf(1, 2)\n\
         \x20   val b = listOf(1, 2)\n\
         \x20   val other = listOf(1, 3)\n\
         \x20   if (a !== a) return \"fail identity\"\n\
         \x20   if (a != b) return \"fail equal\"\n\
         \x20   if (a == other) return \"fail unequal\"\n\
         \x20   if (a.hashCode() != b.hashCode()) return \"fail hash\"\n\
         \x20   if (a.toString() != \"[1, 2]\") return \"fail render: \" + a.toString()\n\
         \x20   if (listOf<Int>().toString() != \"[]\") return \"fail empty render\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "ListValueSemantics",
        "OK",
    );
}

#[test]
fn an_interface_typed_receiver_iterates_whichever_it_holds() {
    // The whole reason the runtime decides from the descriptor. `walk` can say no more about its
    // receiver than `Iterable<Int>`, and both things this runtime can iterate wear that type: read
    // one as the other and a bound becomes a pointer.
    expect_native_box(
        "fun walk(values: Iterable<Int>): Int {\n\
         \x20   var total = 0\n\
         \x20   for (value in values) total += value\n\
         \x20   return total\n\
         }\n\
         fun box(): String {\n\
         \x20   if (walk(listOf(1, 2, 3)) != 6) return \"fail list\"\n\
         \x20   if (walk(1..3) != 6) return \"fail range\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "IterableDispatch",
        "OK",
    );
}

#[test]
fn a_list_holds_its_elements_against_the_collector() {
    // The list's one reference field is the array, and the elements are traced through it. Enough
    // garbage to force collections while the list is the only thing keeping them alive.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val held = listOf(\"a\" + \"b\", \"c\" + \"d\", \"e\" + \"f\")\n\
         \x20   var waste = \"\"\n\
         \x20   var at = 0\n\
         \x20   while (at < 200000) {\n\
         \x20       waste = \"x\" + at\n\
         \x20       at += 1\n\
         \x20   }\n\
         \x20   if (waste == \"\") return \"fail waste\"\n\
         \x20   var joined = \"\"\n\
         \x20   for (s in held) joined += s\n\
         \x20   return if (joined == \"abcdef\") \"OK\" else \"fail: $joined\"\n\
         }\n",
        "ListSurvivesCollection",
        "OK",
    );
}
