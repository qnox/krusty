//! `indices` over the things that are not arrays.
//!
//! `x.indices` is `0..size-1`, and the backend answered it only for an array and a `String`.
//! Everything else declined by name, which the conformance report listed as four distinct
//! receivers: `List<Int>`, `List<String>`, `Collection<*>`, `CharSequence`, and a type parameter
//! bounded by `Collection<*>`.
//!
//! None of them needed anything new from the runtime. A list answers its size through the same
//! entry point `size` itself reaches, and every `CharSequence` this target can produce IS a string
//! — `subSequence` answers one and nothing in the runtime makes another — so it reads its length
//! the way a `String` does. A type parameter stands for whatever its bound admits, and `indices`
//! is declared for the bound.
//!
//! Expectations are kotlinc's, taken by running the same expressions under it.

use super::common::expect_native_box;

/// Each receiver the report named, and the empty case that fixes the range's shape.
///
/// `listOf<Int>().indices` is `0..-1`, not an error and not `0..0`: `indices` is `0` to `size - 1`,
/// and for an empty receiver that is a range whose end precedes its start. A walk over it runs
/// zero times.
#[test]
fn indices_answers_for_a_list_a_char_sequence_and_a_bounded_type_parameter() {
    expect_native_box(
        "fun <T : Collection<*>> viaParam(c: T): String = c.indices.toString()\n\
         fun box(): String {\n\
         \x20   val l = listOf(\"a\", \"b\", \"c\")\n\
         \x20   val empty = listOf<Int>()\n\
         \x20   val cs: CharSequence = \"hello\"\n\
         \x20   if (l.indices.toString() != \"0..2\") return \"fail list: \" + l.indices.toString()\n\
         \x20   if (empty.indices.toString() != \"0..-1\") return \"fail empty: \" + empty.indices.toString()\n\
         \x20   if (cs.indices.toString() != \"0..4\") return \"fail charsequence: \" + cs.indices.toString()\n\
         \x20   if (viaParam(l) != \"0..2\") return \"fail type parameter: \" + viaParam(l)\n\
         \x20   return \"OK\"\n\
         }\n",
        "CollectionIndices",
        "OK",
    );
}

/// The range is one a loop walks, not just one that renders.
///
/// `toString` alone would pass on a range built from the wrong end, so this sums the indices it
/// actually yields: 0 + 1 + 2 for three elements, and nothing at all for an empty receiver.
#[test]
fn a_walk_over_indices_yields_every_position_and_no_more() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val l = listOf(\"a\", \"b\", \"c\")\n\
         \x20   var sum = 0\n\
         \x20   var count = 0\n\
         \x20   for (i in l.indices) { sum = sum + i; count = count + 1 }\n\
         \x20   if (sum != 3) return \"fail sum: \" + sum.toString()\n\
         \x20   if (count != 3) return \"fail count: \" + count.toString()\n\
         \x20   var empties = 0\n\
         \x20   for (i in listOf<Int>().indices) { empties = empties + 1 }\n\
         \x20   if (empties != 0) return \"fail empty walk: \" + empties.toString()\n\
         \x20   val cs: CharSequence = \"hey\"\n\
         \x20   var letters = 0\n\
         \x20   for (i in cs.indices) { letters = letters + 1 }\n\
         \x20   if (letters != 3) return \"fail charsequence walk: \" + letters.toString()\n\
         \x20   return \"OK\"\n\
         }\n",
        "IndicesWalk",
        "OK",
    );
}

/// A `Collection` a SET stands behind answers the set's own size.
///
/// A set is a `Collection` with no list header, so reading one as a list read a field of the set
/// as its size. Its size is how many elements it walks, and that is what `indices` is built from.
#[test]
fn indices_of_a_set_behind_a_collection_answers_the_set_size() {
    expect_native_box(
        "fun <T : Collection<*>> viaParam(c: T): String = c.indices.toString()\n\
         fun box(): String {\n\
         \x20   val s = setOf(\"a\", \"b\", \"c\", \"a\")\n\
         \x20   val c: Collection<String> = s\n\
         \x20   if (c.indices.toString() != \"0..2\") return \"fail collection: \" + c.indices.toString()\n\
         \x20   if (viaParam(s) != \"0..2\") return \"fail type parameter: \" + viaParam(s)\n\
         \x20   if (viaParam(emptySet<Int>()) != \"0..-1\") return \"fail empty: \" + viaParam(emptySet<Int>())\n\
         \x20   return \"OK\"\n\
         }\n",
        "SetIndices",
        "OK",
    );
}
