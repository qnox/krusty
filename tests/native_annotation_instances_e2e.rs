//! Annotation classes as VALUES: `Anno("OK", 42)` constructed, read, compared and rendered.
//!
//! Kotlin defines an annotation instance's `equals`, `hashCode` and `toString` over its MEMBERS
//! rather than by identity, and an ARRAY member is compared, hashed and rendered by CONTENT. That
//! last one is the whole difference from a data class, where an array member is compared by
//! identity — so the two cannot share one synthesis.
//!
//! `hashCode` is a contract a program can read rather than an implementation detail: it is the sum
//! of `(127 * name.hashCode()) xor value.hashCode()` over the members, and the corpus's
//! `annotations/instances/annotationEqHc.kt` computes that sum in Kotlin and compares. The member
//! name's hash is taken at run time so it is the same `String.hashCode` the program's own
//! `name.hashCode()` reaches.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// The two zeroes stay distinct and NaN equals itself — the total order, as an annotation's
/// members are compared through their boxes.
#[test]
fn an_annotation_compares_its_floating_point_members_on_the_total_order() {
    let source = "annotation class F(val f: Float, val d: Double)\n\
         fun box(): String {\n\
         \x20   if (F(Float.NaN, Double.NaN) != F(Float.NaN, Double.NaN)) return \"fail NaN\"\n\
         \x20   if (F(0.0f, 0.0) == F(-0.0f, -0.0)) return \"fail zeroes\"\n\
         \x20   if (F(0.0f, 0.0) != F(0.0f, 0.0)) return \"fail same zero\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "AnnotationFloats");
    expect_native_box(source, "AnnotationFloats", "OK");
}
