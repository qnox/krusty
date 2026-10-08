//! `xs.isEmpty()` on an ARRAY, and `xs.toTypedArray()` on anything walkable.
//!
//! An array's emptiness is its LENGTH, and that is the same question whichever element width it
//! has — a `DoubleArray` answers it the way an `Array<String>` does. `toTypedArray` is the other
//! direction: a reference `Array<T>` holding what the receiver walks, which is a copy rather than
//! a conversion of each element, since a collection's elements are already references.
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

/// Both answers, on a primitive array.
#[test]
fn a_primitive_array_answers_its_emptiness() {
    let source = "fun box(): String {\n\
         \x20   if (!intArrayOf().isEmpty()) return \"fail empty\"\n\
         \x20   if (intArrayOf(1).isEmpty()) return \"fail filled\"\n\
         \x20   if (intArrayOf().isNotEmpty()) return \"fail not empty\"\n\
         \x20   if (!intArrayOf(1).isNotEmpty()) return \"fail not filled\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("PrimitiveArrayEmptiness", source);
}

/// A REFERENCE array reads the same way, and so does a wider element.
#[test]
fn a_reference_array_and_a_wide_one_answer_alike() {
    let source = "fun box(): String {\n\
         \x20   if (!arrayOf<String>().isEmpty()) return \"fail ref empty\"\n\
         \x20   if (arrayOf(\"a\").isEmpty()) return \"fail ref filled\"\n\
         \x20   if (!doubleArrayOf().isEmpty()) return \"fail double empty\"\n\
         \x20   if (doubleArrayOf(1.0).isEmpty()) return \"fail double filled\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ReferenceArrayEmptiness", source);
}

/// `toTypedArray` on a LIST, read back by index.
#[test]
fn a_list_becomes_a_typed_array() {
    let source = "fun box(): String {\n\
         \x20   val array = listOf(\"O\", \"K\").toTypedArray()\n\
         \x20   if (array.size != 2) return \"fail size \" + array.size\n\
         \x20   return array[0] + array[1]\n\
         }\n";
    every_backend_agrees_with_kotlinc("ListToTypedArray", source);
}

/// An EMPTY receiver gives an empty array rather than nothing.
#[test]
fn an_empty_list_becomes_an_empty_array() {
    let source = "fun box(): String {\n\
         \x20   val array = listOf<String>().toTypedArray()\n\
         \x20   return if (array.isEmpty()) \"OK\" else \"fail \" + array.size\n\
         }\n";
    every_backend_agrees_with_kotlinc("EmptyListToTypedArray", source);
}

/// The array is a COPY: writing through it does not reach the list it came from.
#[test]
fn the_typed_array_is_a_copy_of_what_it_walked() {
    let source = "fun box(): String {\n\
         \x20   val source = mutableListOf(\"a\")\n\
         \x20   val array = source.toTypedArray()\n\
         \x20   array[0] = \"b\"\n\
         \x20   if (source[0] != \"a\") return \"fail source \" + source[0]\n\
         \x20   if (array[0] != \"b\") return \"fail array \" + array[0]\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("TypedArrayIsACopy", source);
}

/// A SET walks too, so it converts the same way.
#[test]
fn a_set_becomes_a_typed_array() {
    let source = "fun box(): String {\n\
         \x20   val array = setOf(\"OK\").toTypedArray()\n\
         \x20   return if (array.size == 1) array[0] else \"fail \" + array.size\n\
         }\n";
    every_backend_agrees_with_kotlinc("SetToTypedArray", source);
}
