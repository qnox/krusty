//! The runtime walking a collection the PROGRAM declared.
//!
//! Every walking entry point of the runtime — `withIndex`, `contains`, `map`, `joinToString` —
//! reaches its elements through `iterator()`, `hasNext()` and `next()`, and those knew only the
//! shapes the runtime itself builds. A class of the program puts its own members at a vtable slot
//! assigned per program, which the runtime cannot guess; its descriptor records the three numbers
//! instead, and the walk dispatches through them. So a stdlib extension over a collection of the
//! program's is the same answer it is over a list.
//!
//! Laziness is what these programs check beyond the result: Kotlin's `withIndex` asks for one
//! element at a time, and a counting iterable can see whether that held.
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

/// A class of the program declaring both halves of the walk: an `Iterable` and the `Iterator` it
/// hands out. `withIndex` counts as it goes, and the counting iterable sees exactly one `next` per
/// element and one `hasNext` beyond them.
#[test]
fn a_programs_own_iterable_is_walked_with_index() {
    let source = "class Counting(private val source: List<String>) : Iterable<String> {\n\
         \x20   var hasNextCalls = 0\n\
         \x20   var nextCalls = 0\n\
         \x20   inner class Walk(private val inner: Iterator<String>) : Iterator<String> {\n\
         \x20       override fun hasNext(): Boolean {\n\
         \x20           hasNextCalls++\n\
         \x20           return inner.hasNext()\n\
         \x20       }\n\
         \x20       override fun next(): String {\n\
         \x20           nextCalls++\n\
         \x20           return inner.next()\n\
         \x20       }\n\
         \x20   }\n\
         \x20   override fun iterator(): Iterator<String> = Walk(source.iterator())\n\
         }\n\
         fun box(): String {\n\
         \x20   val xs = Counting(listOf(\"a\", \"b\", \"c\"))\n\
         \x20   var seen = \"\"\n\
         \x20   for (entry in xs.withIndex()) {\n\
         \x20       seen += \"${entry.index}:${entry.value};\"\n\
         \x20   }\n\
         \x20   if (seen != \"0:a;1:b;2:c;\") return \"fail walk \" + seen\n\
         \x20   if (xs.hasNextCalls != 4) return \"fail hasNext \" + xs.hasNextCalls\n\
         \x20   if (xs.nextCalls != 3) return \"fail next \" + xs.nextCalls\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("WalkableWithIndex", source);
}

/// The other walking entry points over the same shape: a question, a transform and a rendering.
#[test]
fn a_programs_own_iterable_answers_the_walking_members() {
    let source = "class Letters(private val source: List<String>) : Iterable<String> {\n\
         \x20   override fun iterator(): Iterator<String> = source.iterator()\n\
         }\n\
         fun box(): String {\n\
         \x20   val xs = Letters(listOf(\"O\", \"K\"))\n\
         \x20   if (!xs.contains(\"O\")) return \"fail contains\"\n\
         \x20   if (xs.contains(\"z\")) return \"fail absent\"\n\
         \x20   if (xs.count() != 2) return \"fail count \" + xs.count()\n\
         \x20   if (xs.joinToString() != \"O, K\") return \"fail join \" + xs.joinToString()\n\
         \x20   val doubled = xs.map { it + it }\n\
         \x20   if (doubled != listOf(\"OO\", \"KK\")) return \"fail map \" + doubled\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("WalkableMembers", source);
}

/// A SUBCLASS of a walkable class is walked through the same members: a slot number assigned at
/// the declaring class is valid for every subclass, and the subclass's descriptor carries it.
#[test]
fn a_subclass_of_a_walkable_class_is_walked_too() {
    let source = "open class Base(private val source: List<String>) : Iterable<String> {\n\
         \x20   override fun iterator(): Iterator<String> = source.iterator()\n\
         }\n\
         class Derived(source: List<String>) : Base(source)\n\
         fun box(): String {\n\
         \x20   val xs = Derived(listOf(\"O\", \"K\"))\n\
         \x20   return if (xs.joinToString() == \"O, K\") \"OK\" else \"fail \" + xs.joinToString()\n\
         }\n";
    every_backend_agrees_with_kotlinc("WalkableSubclass", source);
}

/// The same, through a GENERIC iterable whose iterator is an INNER class, and reached with the
/// element's type left to inference: the shape the corpus writes, and the one that shows whether
/// the thunk dispatches on the class an object really has rather than on the one named here.
#[test]
fn a_generic_iterable_with_an_inner_iterator_is_walked() {
    let source = "class Counting<out T>(private val s: Iterable<T>) : Iterable<T> {\n\
         \x20   var hasNextCtr = 0\n\
         \x20   var nextCtr = 0\n\
         \x20   inner class Walk(private val it: Iterator<T>) : Iterator<T> {\n\
         \x20       override fun hasNext() = it.hasNext().also { hasNextCtr++ }\n\
         \x20       override fun next() = it.next().also { nextCtr++ }\n\
         \x20   }\n\
         \x20   override fun iterator() = Walk(s.iterator())\n\
         }\n\
         fun box(): String {\n\
         \x20   val xs = Counting(listOf(\"a\", \"b\"))\n\
         \x20   var seen = \"\"\n\
         \x20   for (entry in xs.withIndex()) {\n\
         \x20       seen += \"${entry.index}:${entry.value};\"\n\
         \x20   }\n\
         \x20   if (seen != \"0:a;1:b;\") return \"fail walk \" + seen\n\
         \x20   if (xs.hasNextCtr != 3) return \"fail hasNext \" + xs.hasNextCtr\n\
         \x20   if (xs.nextCtr != 2) return \"fail next \" + xs.nextCtr\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("WalkableGenericInner", source);
}
