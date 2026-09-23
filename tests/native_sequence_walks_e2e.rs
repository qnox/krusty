//! A class of the PROGRAM that implements `kotlin.sequences.Sequence`.
//!
//! Such a class declares exactly one member — `iterator()` — and the runtime walks an object of it
//! through the very thunk it walks a program's `Iterable` through: the descriptor carries the slot,
//! and the dispatch is virtual, so who made the object never matters. What the sequence shape does
//! change is which MEMBERS the receiver may be asked. Every walk this runtime has is eager, and an
//! eager `map` over a sequence is not Kotlin's — the transform would run for every element where
//! Kotlin runs it per element consumed. So the narrowing a receiver typed `Sequence` already
//! carried now follows the program's own class too, and the members that are lazy either way —
//! its iterator, and `withIndex` — are the ones answered.
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

/// A sequence of the program's own, whose iterator is an INNER class — the corpus shape.
const COUNTING: &str =
    "class CountingSequence<out T>(private val s: Sequence<T>) : Sequence<T> {\n\
     \x20   var hasNextCtr = 0\n\
     \x20   var nextCtr = 0\n\
     \x20   inner class Walk(private val it: Iterator<T>) : Iterator<T> {\n\
     \x20       override fun hasNext() = it.hasNext().also { hasNextCtr++ }\n\
     \x20       override fun next() = it.next().also { nextCtr++ }\n\
     \x20   }\n\
     \x20   override fun iterator() = Walk(s.iterator())\n\
     }\n";

/// A `for` over a program sequence walks it.
#[test]
fn a_program_sequence_is_walked() {
    let source = format!(
        "{COUNTING}\
         fun box(): String {{\n\
         \x20   val s = StringBuilder()\n\
         \x20   for (x in CountingSequence(listOf(\"a\", \"b\").asSequence())) s.append(x)\n\
         \x20   return if (s.toString() == \"ab\") \"OK\" else \"fail \" + s.toString()\n\
         }}\n"
    );
    every_backend_agrees_with_kotlinc("ProgramSequenceWalk", &source);
}

/// `withIndex()` over one, which is the member the corpus cases ask for.
#[test]
fn a_program_sequence_answers_with_index() {
    let source = format!(
        "{COUNTING}\
         fun box(): String {{\n\
         \x20   val s = StringBuilder()\n\
         \x20   for (pair in CountingSequence(listOf(\"a\", \"b\", \"c\").asSequence()).withIndex()) {{\n\
         \x20       s.append(\"\" + pair.index + \":\" + pair.value + \";\")\n\
         \x20   }}\n\
         \x20   return if (s.toString() == \"0:a;1:b;2:c;\") \"OK\" else \"fail \" + s.toString()\n\
         }}\n"
    );
    every_backend_agrees_with_kotlinc("ProgramSequenceWithIndex", &source);
}

/// The walk is the PROGRAM's members, counted: the side effects its iterator records are the ones
/// Kotlin's walk produces, one `hasNext` past the last element and one `next` per element.
#[test]
fn the_walk_runs_the_programs_own_iterator() {
    let source = format!(
        "{COUNTING}\
         fun box(): String {{\n\
         \x20   val xs = CountingSequence(listOf(\"a\", \"b\", \"c\", \"d\").asSequence())\n\
         \x20   for (pair in xs.withIndex()) {{ pair.value }}\n\
         \x20   if (xs.hasNextCtr != 5) return \"fail hasNext \" + xs.hasNextCtr\n\
         \x20   if (xs.nextCtr != 4) return \"fail next \" + xs.nextCtr\n\
         \x20   return \"OK\"\n\
         }}\n"
    );
    every_backend_agrees_with_kotlinc("ProgramSequenceSideEffects", &source);
}

/// A DESTRUCTURING loop over the indexed walk, which is how the corpus writes it.
#[test]
fn the_indexed_walk_destructures() {
    let source = format!(
        "{COUNTING}\
         fun box(): String {{\n\
         \x20   val s = StringBuilder()\n\
         \x20   for ((index, x) in CountingSequence(listOf(\"a\", \"b\").asSequence()).withIndex()) {{\n\
         \x20       s.append(\"\" + index + x)\n\
         \x20   }}\n\
         \x20   return if (s.toString() == \"0a1b\") \"OK\" else \"fail \" + s.toString()\n\
         }}\n"
    );
    every_backend_agrees_with_kotlinc("ProgramSequenceDestructured", &source);
}

/// A SUBCLASS of one is walked through the same slot: the thunk belongs to the class that declares
/// `iterator`, and a subclass overriding it is reached through that very thunk.
#[test]
fn a_subclass_of_a_program_sequence_walks_too() {
    let source = "open class Letters : Sequence<String> {\n\
         \x20   override fun iterator() = listOf(\"a\", \"b\").iterator()\n\
         }\n\
         class Louder : Letters() {\n\
         \x20   override fun iterator() = listOf(\"A\", \"B\").iterator()\n\
         }\n\
         fun box(): String {\n\
         \x20   val s = StringBuilder()\n\
         \x20   for (x in Louder()) s.append(x)\n\
         \x20   return if (s.toString() == \"AB\") \"OK\" else \"fail \" + s.toString()\n\
         }\n";
    every_backend_agrees_with_kotlinc("ProgramSequenceSubclass", source);
}

/// A receiver typed by `Sequence` ITSELF, holding an object of the program's: the walk is the
/// descriptor's either way, so naming the interface loses nothing.
#[test]
fn the_interface_type_reaches_the_programs_object() {
    let source = "class Letters : Sequence<String> {\n\
         \x20   override fun iterator() = listOf(\"a\", \"b\").iterator()\n\
         }\n\
         fun walk(xs: Sequence<String>): String {\n\
         \x20   val s = StringBuilder()\n\
         \x20   for (x in xs) s.append(x)\n\
         \x20   return s.toString()\n\
         }\n\
         fun box(): String {\n\
         \x20   val said = walk(Letters())\n\
         \x20   return if (said == \"ab\") \"OK\" else \"fail \" + said\n\
         }\n";
    every_backend_agrees_with_kotlinc("ProgramSequenceThroughInterface", source);
}

/// The narrowing holds for the program's class as it does for the interface: an EAGER walk of a
/// sequence is not Kotlin's, so `map` declines rather than running the transform for every element.
#[test]
fn an_eager_member_of_a_program_sequence_declines() {
    let source = "class Letters : Sequence<String> {\n\
         \x20   override fun iterator() = listOf(\"a\", \"b\").iterator()\n\
         }\n\
         fun box(): String {\n\
         \x20   val loud = Letters().map { it.uppercase() }\n\
         \x20   return if (loud.first() == \"A\") \"OK\" else \"fail\"\n\
         }\n";
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "EagerSequenceMember: unexpected kotlinc result"
    );
    expect_native_decline(source, "EagerSequenceMember", "map");
}
