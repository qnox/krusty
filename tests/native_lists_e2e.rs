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

#[test]
fn a_single_element_list_is_not_a_vararg_call_of_length_one() {
    // Kotlin declares `listOf` twice, and which one a call selects decides whether its argument IS
    // the elements or is one OF them. `listOf(anArray)` takes the single-element overload and
    // answers a list of one array — reading the argument's type instead of the declaration's would
    // unpack it and answer a list of three `Int`s.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val one = listOf(\"only\")\n\
         \x20   if (one.size != 1) return \"fail size: \" + one.size\n\
         \x20   if (one[0] != \"only\") return \"fail element\"\n\
         \x20   val held = listOf(arrayOf(1, 2, 3))\n\
         \x20   if (held.size != 1) return \"fail array size: \" + held.size\n\
         \x20   if (held[0].size != 3) return \"fail inner\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "SingleElementList",
        "OK",
    );
}

#[test]
fn a_pair_carries_two_values_and_answers_by_them() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val p = \"a\" to 1\n\
         \x20   if (p.first != \"a\") return \"fail first\"\n\
         \x20   if (p.second != 1) return \"fail second\"\n\
         \x20   if (p.toString() != \"(a, 1)\") return \"fail render: \" + p.toString()\n\
         \x20   if (p != (\"a\" to 1)) return \"fail equal\"\n\
         \x20   if (p == (\"a\" to 2)) return \"fail unequal\"\n\
         \x20   if (p.hashCode() != (\"a\" to 1).hashCode()) return \"fail hash\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "PairMembers",
        "OK",
    );
}

#[test]
fn a_pair_destructures_through_its_components() {
    // `component1`/`component2` are the same two questions under the names the convention uses.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val (name, count) = \"x\" to 3\n\
         \x20   if (name != \"x\") return \"fail name\"\n\
         \x20   if (count != 3) return \"fail count\"\n\
         \x20   var joined = \"\"\n\
         \x20   for ((key, value) in listOf(\"a\" to 1, \"b\" to 2)) joined += key + value\n\
         \x20   return if (joined == \"a1b2\") \"OK\" else \"fail: $joined\"\n\
         }\n",
        "PairDestructuring",
        "OK",
    );
}

#[test]
fn a_lazy_value_is_computed_once_and_only_when_asked() {
    // Both halves matter and neither is visible from the value alone: the initializer must not run
    // before the first read, and must not run again after it.
    expect_native_box(
        "var runs = 0\n\
         val greeting: String by lazy {\n\
         \x20   runs += 1\n\
         \x20   \"O\" + \"K\"\n\
         }\n\
         fun box(): String {\n\
         \x20   if (runs != 0) return \"fail eager: $runs\"\n\
         \x20   val first = greeting\n\
         \x20   if (runs != 1) return \"fail first: $runs\"\n\
         \x20   val second = greeting\n\
         \x20   if (runs != 1) return \"fail again: $runs\"\n\
         \x20   if (first != second) return \"fail differs\"\n\
         \x20   return first\n\
         }\n",
        "LazyOnce",
        "OK",
    );
}

#[test]
fn a_lazy_initializer_reads_what_it_captured() {
    // The runtime calls back into emitted code to compute the value, so the initializer has to
    // arrive as the closure it is — captures and all — rather than as a bare function pointer.
    expect_native_box(
        "class Holder(private val part: String) {\n\
         \x20   val whole: String by lazy { part + \"K\" }\n\
         }\n\
         fun box(): String {\n\
         \x20   val outer = \"O\"\n\
         \x20   val local: String by lazy { outer + \"K\" }\n\
         \x20   if (Holder(\"O\").whole != \"OK\") return \"fail member\"\n\
         \x20   return local\n\
         }\n",
        "LazyCaptures",
        "OK",
    );
}

#[test]
fn a_lazy_holds_its_value_and_answers_before_it_has_one() {
    // `Lazy.toString` must not force the value — that is the whole point of its wording — and the
    // computed value has to survive collection, being reachable only through the lazy.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val held = lazy { \"a\" + \"b\" }\n\
         \x20   if (held.isInitialized()) return \"fail early\"\n\
         \x20   if (held.toString() != \"Lazy value not initialized yet.\") return \"fail render\"\n\
         \x20   if (held.value != \"ab\") return \"fail value\"\n\
         \x20   if (!held.isInitialized()) return \"fail late\"\n\
         \x20   if (held.toString() != \"ab\") return \"fail rendered value\"\n\
         \x20   var waste = \"\"\n\
         \x20   var at = 0\n\
         \x20   while (at < 200000) {\n\
         \x20       waste = \"x\" + at\n\
         \x20       at += 1\n\
         \x20   }\n\
         \x20   if (waste == \"\") return \"fail waste\"\n\
         \x20   return if (held.value == \"ab\") \"OK\" else \"fail survived\"\n\
         }\n",
        "LazyValueSurvives",
        "OK",
    );
}

#[test]
fn map_and_for_each_walk_whichever_iterable_they_are_handed() {
    // Both are declared on `Iterable`, so a receiver typed by it may hold either of the two things
    // this runtime can iterate. The runtime decides from the descriptor — the same decision
    // iteration itself makes — so a range is walked as readily as a list.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val doubled = listOf(1, 2, 3).map { it * 2 }\n\
         \x20   if (doubled != listOf(2, 4, 6)) return \"fail list map: $doubled\"\n\
         \x20   val counted = (1..4).map { it + 10 }\n\
         \x20   if (counted != listOf(11, 12, 13, 14)) return \"fail range map: $counted\"\n\
         \x20   if (listOf<Int>().map { it }.size != 0) return \"fail empty map\"\n\
         \x20   if ((1..0).map { it }.size != 0) return \"fail empty range map\"\n\
         \x20   var total = 0\n\
         \x20   listOf(1, 2, 3).forEach { total += it }\n\
         \x20   (1..4).forEach { total += it }\n\
         \x20   return if (total == 16) \"OK\" else \"fail forEach: $total\"\n\
         }\n",
        "MapAndForEach",
        "OK",
    );
}

#[test]
fn a_mapped_list_holds_what_the_transform_made() {
    // Every element is allocated by a call the loop makes, and each such call may collect — so the
    // result has to be a root while it is still being filled.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val built = (1..200).map { \"item\" + it }\n\
         \x20   var waste = \"\"\n\
         \x20   var at = 0\n\
         \x20   while (at < 200000) {\n\
         \x20       waste = \"x\" + at\n\
         \x20       at += 1\n\
         \x20   }\n\
         \x20   if (waste == \"\") return \"fail waste\"\n\
         \x20   if (built.size != 200) return \"fail size: \" + built.size\n\
         \x20   if (built[0] != \"item1\") return \"fail first: \" + built[0]\n\
         \x20   return if (built[199] == \"item200\") \"OK\" else \"fail last: \" + built[199]\n\
         }\n",
        "MappedListSurvives",
        "OK",
    );
}

#[test]
fn a_list_joins_to_a_string_with_the_default_separator() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   if (listOf(1, 2, 3).joinToString() != \"1, 2, 3\") return \"fail ints\"\n\
         \x20   if (listOf(\"a\").joinToString() != \"a\") return \"fail single\"\n\
         \x20   if (emptyList<Int>().joinToString() != \"\") return \"fail empty\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "JoinToStringDefaults",
        "OK",
    );
}

#[test]
fn joining_renders_each_element_through_its_own_to_string() {
    expect_native_box(
        "class Tag(val text: String) {\n\
         \x20   override fun toString(): String = \"<$text>\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val joined = listOf(Tag(\"a\"), Tag(\"b\")).joinToString()\n\
         \x20   return if (joined == \"<a>, <b>\") \"OK\" else \"fail: $joined\"\n\
         }\n",
        "JoinToStringOverride",
        "OK",
    );
}

#[test]
fn a_range_joins_the_same_way_a_list_does() {
    // `joinToString` is declared on `Iterable`, which both of the runtime's iterables wear, so it
    // goes through the same descriptor dispatch iteration itself does.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val joined = (1..3).joinToString()\n\
         \x20   return if (joined == \"1, 2, 3\") \"OK\" else \"fail: $joined\"\n\
         }\n",
        "JoinToStringRange",
        "OK",
    );
}

#[test]
fn joining_with_an_argument_still_declines() {
    // Every parameter of `joinToString` is defaulted and this backend has no `$default` synthetic
    // of a dependency to call, so a call that passes one has nothing to route to. Declining keeps
    // the argument in sight rather than dropping it.
    super::common::expect_native_decline(
        "fun box(): String = listOf(1, 2, 3).joinToString(\"-\")\n",
        "JoinToStringWithSeparator",
        "joinToString",
    );
}
