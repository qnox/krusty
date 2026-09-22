//! `xs.asSequence()` — the source kept until something asks it for an iterator.
//!
//! Lazy, as Kotlin's is, and its own runtime type rather than the source itself. What separates a
//! `Sequence` from an `Iterable` here is which members may be asked of it: every walk this runtime
//! has is EAGER, and an eager `map` on a sequence is not Kotlin's — the transform would run for
//! every element where Kotlin runs it per element consumed, which a side effect sees and an endless
//! sequence never survives. So a sequence is offered only the members whose answer is the same
//! either way, its iterator and the lazy `withIndex`, and the rest decline at the call site where
//! the type the program named is still in sight.
//!
//! Its `equals` and `hashCode` are IDENTITY, which is what Kotlin answers: `Sequence` declares
//! neither, so two sequences over the same elements are different objects.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, expect_native_decline};

/// A sequence over each iterable this runtime has, walked and indexed.
#[test]
fn a_sequence_walks_whatever_it_was_made_from() {
    let source = "fun box(): String {\n\
         \x20   var walked = \"\"\n\
         \x20   for (x in listOf(1, 2, 3).asSequence()) walked += x\n\
         \x20   if (walked != \"123\") return \"fail list \" + walked\n\
         \x20   var ranged = \"\"\n\
         \x20   for (x in (1..3).asSequence()) ranged += x\n\
         \x20   if (ranged != \"123\") return \"fail range \" + ranged\n\
         \x20   var arrayed = \"\"\n\
         \x20   for (x in intArrayOf(4, 5).asSequence()) arrayed += x\n\
         \x20   if (arrayed != \"45\") return \"fail array \" + arrayed\n\
         \x20   // Text is iterable here too, so a sequence over it walks its characters.\n\
         \x20   var lettered = \"\"\n\
         \x20   for (c in \"ab\".asSequence()) lettered += c\n\
         \x20   if (lettered != \"ab\") return \"fail text \" + lettered\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "SequenceWalks");
    expect_native_box(source, "SequenceWalks", "OK");
}

/// `withIndex()` over a sequence, which is the shape the corpus's `forInSequenceWithIndex` cases
/// are written in — and it stays lazy, so a loop that stops early stops the walk.
#[test]
fn a_sequence_is_indexed_lazily() {
    let source = "val xs = listOf(\"a\", \"b\", \"c\", \"d\").asSequence()\n\
         fun box(): String {\n\
         \x20   val s = StringBuilder()\n\
         \x20   for ((index, x) in xs.withIndex()) s.append(\"\" + index + \":\" + x + \";\")\n\
         \x20   if (s.toString() != \"0:a;1:b;2:c;3:d;\") return \"fail \" + s\n\
         \x20   // A walk that stops early asks for no more than it read.\n\
         \x20   var seen = 0\n\
         \x20   for ((index, _) in xs.withIndex()) {\n\
         \x20       seen++\n\
         \x20       if (index == 1) break\n\
         \x20   }\n\
         \x20   if (seen != 2) return \"fail early \" + seen\n\
         \x20   // The same sequence walks again from the start: it holds a source, not a cursor.\n\
         \x20   var again = \"\"\n\
         \x20   for (x in xs) again += x\n\
         \x20   if (again != \"abcd\") return \"fail again \" + again\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "SequenceIndexed");
    expect_native_box(source, "SequenceIndexed", "OK");
}

/// An EAGER walk over a sequence declines. Answering it would run the transform for every element
/// where Kotlin runs it per element consumed — a different program, not a slower one.
#[test]
fn an_eager_walk_over_a_sequence_declines() {
    expect_native_decline(
        "fun box(): String {\n\
         \x20   val mapped = listOf(1, 2).asSequence().map { it * 2 }\n\
         \x20   var total = 0\n\
         \x20   for (x in mapped) total += x\n\
         \x20   return if (total == 6) \"OK\" else \"fail\"\n\
         }\n",
        "EagerOverSequence",
        "map",
    );
}

/// A file that declares its own `Sequence` walks it — and the list beside it too.
///
/// Two things had to give before this ran. `iterator` takes no arguments, so it is dispatched on
/// the receiver (see `tests/native_implemented_dependency_dispatch_e2e.rs`): the file knows the
/// class of its own that could stand behind a `Sequence`, tests for it, and falls through to the
/// runtime otherwise — which is the arm `source.iterator()` takes here, `source` being a sequence
/// the runtime made. And the guard that used to block the OTHER half is a question about the
/// receiver now rather than about the file: declaring a `Sequence` endangers a sequence receiver
/// and nothing else, so `listOf(…).asSequence()` in the same file is answered as it always was.
#[test]
fn a_file_that_declares_its_own_sequence_walks_it() {
    let source = "class Counting<out T>(private val source: Sequence<T>) : Sequence<T> {\n\
         \x20   override fun iterator() = source.iterator()\n\
         }\n\
         fun box(): String {\n\
         \x20   var text = \"\"\n\
         \x20   for (x in Counting(listOf(\"O\", \"K\").asSequence())) text += x\n\
         \x20   return text\n\
         }\n";
    expect_box_ok_with_stdlib(source, "DeclaredSequence");
    expect_native_box(source, "DeclaredSequence", "OK");
}

/// A file that declares its own `Sequence` still declines a SEQUENCE receiver's other members.
///
/// The shape the file implements is the sequence's, and `withIndex` is not a nullary member the
/// receiver test can carry — so the decline stands exactly where the hazard is.
#[test]
fn a_declared_sequence_still_declines_the_members_it_endangers() {
    expect_native_decline(
        "class Counting<out T>(private val source: Sequence<T>) : Sequence<T> {\n\
         \x20   override fun iterator() = source.iterator()\n\
         }\n\
         fun box(): String {\n\
         \x20   val xs: Sequence<String> = Counting(listOf(\"O\", \"K\").asSequence())\n\
         \x20   var text = \"\"\n\
         \x20   for ((_, x) in xs.withIndex()) text += x\n\
         \x20   return text\n\
         }\n",
        "DeclaredSequenceIndexed",
        "withIndex",
    );
}
