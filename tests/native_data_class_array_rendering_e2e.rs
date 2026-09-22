//! A data class whose property is an ARRAY, rendered by its generated `toString`.
//!
//! Kotlin shows the array's CONTENTS there — `data class D(val xs: IntArray)` prints
//! `D(xs=[1, 2])` — not the identity an array's own `toString` answers. It is the same rendering
//! `xs.contentToString()` asks for, so it reaches the same runtime entry point.
//!
//! Only `toString` is special this way. `equals` and `hashCode` on a data class with an array
//! property stay the array's own, which is identity — that is Kotlin's rule, not an omission, and
//! two of the tests below pin it so the rendering change cannot quietly take them with it.
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

/// A primitive array property shows its elements.
#[test]
fn a_data_class_renders_a_primitive_arrays_contents() {
    let source = "data class D(val xs: IntArray)\n\
         fun box(): String {\n\
         \x20   val text = D(intArrayOf(1, 2)).toString()\n\
         \x20   return if (text == \"D(xs=[1, 2])\") \"OK\" else \"fail \" + text\n\
         }\n";
    every_backend_agrees_with_kotlinc("DataClassIntArrayToString", source);
}

/// A REFERENCE array property does too.
#[test]
fn a_data_class_renders_a_reference_arrays_contents() {
    let source = "data class D(val xs: Array<String>)\n\
         fun box(): String {\n\
         \x20   val text = D(arrayOf(\"a\", \"b\")).toString()\n\
         \x20   return if (text == \"D(xs=[a, b])\") \"OK\" else \"fail \" + text\n\
         }\n";
    every_backend_agrees_with_kotlinc("DataClassRefArrayToString", source);
}

/// An EMPTY array, where the brackets are the whole of the rendering.
#[test]
fn a_data_class_renders_an_empty_array() {
    let source = "data class D(val xs: IntArray)\n\
         fun box(): String {\n\
         \x20   val text = D(intArrayOf()).toString()\n\
         \x20   return if (text == \"D(xs=[])\") \"OK\" else \"fail \" + text\n\
         }\n";
    every_backend_agrees_with_kotlinc("DataClassEmptyArrayToString", source);
}

/// An array BESIDE ordinary properties, so the rendering is one field among several.
#[test]
fn an_array_renders_beside_the_other_properties() {
    let source = "data class D(val name: String, val xs: IntArray, val count: Int)\n\
         fun box(): String {\n\
         \x20   val text = D(\"n\", intArrayOf(7), 3).toString()\n\
         \x20   return if (text == \"D(name=n, xs=[7], count=3)\") \"OK\" else \"fail \" + text\n\
         }\n";
    every_backend_agrees_with_kotlinc("DataClassArrayAmongFields", source);
}

/// `equals` is NOT the contents: Kotlin leaves an array property's comparison as the array's own,
/// which is identity. Two data classes holding equal contents are therefore unequal.
#[test]
fn equality_on_an_array_property_stays_identity() {
    let source = "data class D(val xs: IntArray)\n\
         fun box(): String {\n\
         \x20   val a = D(intArrayOf(1))\n\
         \x20   val b = D(intArrayOf(1))\n\
         \x20   if (a == b) return \"fail equal\"\n\
         \x20   if (a != a) return \"fail self\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("DataClassArrayEquality", source);
}

/// A data class in a LIST renders through the same path, since the list asks each element.
#[test]
fn an_array_property_renders_through_an_enclosing_list() {
    let source = "data class D(val xs: IntArray)\n\
         fun box(): String {\n\
         \x20   val text = listOf(D(intArrayOf(1, 2))).toString()\n\
         \x20   return if (text == \"[D(xs=[1, 2])]\") \"OK\" else \"fail \" + text\n\
         }\n";
    every_backend_agrees_with_kotlinc("DataClassArrayInList", source);
}
