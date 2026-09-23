//! `Comparator { a, b -> … }` — a functional interface the RUNTIME knows.
//!
//! A `fun interface` declared in this file becomes an object wearing that interface's table, so a
//! caller reaches its member through a program-wide member number. `kotlin.Comparator` needs none:
//! nothing but its single `compare` is ever asked of it, and every caller is either the runtime or a
//! call site that can see the type. So the conversion changes nothing about the object — it stays
//! the ordinary FUNCTION VALUE the lambda already is, answering through the one invoke slot every
//! function value declares.
//!
//! That is what lets `sortWith` work with no new calling convention: the runtime's sort invokes the
//! comparator exactly as `map` invokes a transform. The sort is STABLE, as Kotlin's is, and
//! stability is observable — two elements the comparator calls equal keep the order they were in.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// A comparator written as a SAM literal, sorting a mutable list in place.
#[test]
fn a_comparator_literal_sorts_a_list_in_place() {
    let source = "fun box(): String {\n\
         \x20   val list = mutableListOf(3, 2, 4, 8, 1, 5)\n\
         \x20   list.sortWith(Comparator { a, b -> b - a })\n\
         \x20   if (list != listOf(8, 5, 4, 3, 2, 1)) return \"fail descending \" + list\n\
         \x20   list.sortWith(Comparator { a, b -> a - b })\n\
         \x20   return if (list == listOf(1, 2, 3, 4, 5, 8)) \"OK\" else \"fail ascending \" + list\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ComparatorLiteralSort");
    expect_native_box(source, "ComparatorLiteralSort", "OK");
}

/// The SAM CONSTRUCTOR over a function value that already exists, rather than over a literal.
#[test]
fn a_comparator_is_built_from_a_function_value() {
    let source = "fun compare(a: String, b: String) = a.compareTo(b)\n\
         fun sort(list: MutableList<String>, comparator: (String, String) -> Int) {\n\
         \x20   list.sortWith(Comparator(comparator))\n\
         }\n\
         fun box(): String {\n\
         \x20   val l = mutableListOf(\"d\", \"b\", \"c\", \"e\", \"a\")\n\
         \x20   sort(l, ::compare)\n\
         \x20   return if (l == listOf(\"a\", \"b\", \"c\", \"d\", \"e\")) \"OK\" else \"fail \" + l\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ComparatorFromFunctionValue");
    expect_native_box(source, "ComparatorFromFunctionValue", "OK");
}

/// `compare` called DIRECTLY, through a receiver typed only by `Comparator`.
#[test]
fn a_comparator_answers_compare_through_its_own_type() {
    let source =
        "fun <T> foo(comparator: Comparator<in T>, a: T, b: T) = comparator.compare(a, b)\n\
         fun bar(x: Int, y: Int) = foo<Int>({ a, b -> a - b }, x, y)\n\
         fun box(): String {\n\
         \x20   if (bar(42, 117) >= 0) return \"fail less\"\n\
         \x20   if (bar(117, 42) <= 0) return \"fail greater\"\n\
         \x20   if (bar(7, 7) != 0) return \"fail equal\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ComparatorCompareDirectly");
    expect_native_box(source, "ComparatorCompareDirectly", "OK");
}

/// `sortedWith` answers a NEW list and leaves the receiver alone, and the sort is STABLE: two
/// elements the comparator calls equal keep the order they were in.
#[test]
fn sorted_with_answers_a_new_list_and_is_stable() {
    let source = "class Pair2(val key: Int, val tag: String)\n\
         fun box(): String {\n\
         \x20   val source = listOf(\n\
         \x20       Pair2(1, \"a\"), Pair2(0, \"b\"), Pair2(1, \"c\"), Pair2(0, \"d\"), Pair2(1, \"e\")\n\
         \x20   )\n\
         \x20   val sorted = source.sortedWith(Comparator { x, y -> x.key - y.key })\n\
         \x20   var order = \"\"\n\
         \x20   for (p in sorted) order += p.tag\n\
         \x20   if (order != \"bdace\") return \"fail stability \" + order\n\
         \x20   var untouched = \"\"\n\
         \x20   for (p in source) untouched += p.tag\n\
         \x20   return if (untouched == \"abcde\") \"OK\" else \"fail receiver \" + untouched\n\
         }\n";
    expect_box_ok_with_stdlib(source, "SortedWithStable");
    expect_native_box(source, "SortedWithStable", "OK");
}
