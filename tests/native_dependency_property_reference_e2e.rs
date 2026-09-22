//! `"abc"::length`, `String::length` — a reference to a DEPENDENCY property.
//!
//! The object is the one a reference to a property of this file becomes: one emitted type per
//! property, `get`, `set` and `name` in its table, and the bound receiver in its one field. What
//! differs is where `get` and `set` lead. A dependency property has no storage here and no
//! accessor of this file's to name, so a native IR pass synthesizes a pair of functions that reach
//! it through the ordinary dependency-property path, and the object is built from those.
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

/// A BOUND reference to a dependency property, whose receiver the object carries.
#[test]
fn a_bound_reference_to_a_dependency_property_reads_it() {
    let source = "fun box(): String {\n\
         \x20   val length = \"abc\"::length\n\
         \x20   return if (length.get() == 3) \"OK\" else \"fail \" + length.get()\n\
         }\n";
    every_backend_agrees_with_kotlinc("BoundDependencyProperty", source);
}

/// An UNBOUND one, whose receiver `get` is handed.
#[test]
fn an_unbound_reference_to_a_dependency_property_reads_it() {
    let source = "fun box(): String {\n\
         \x20   val length = String::length\n\
         \x20   return if (length.get(\"abcd\") == 4) \"OK\" else \"fail \" + length.get(\"abcd\")\n\
         }\n";
    every_backend_agrees_with_kotlinc("UnboundDependencyProperty", source);
}

/// `KCallable.name` answers the property's Kotlin name, which the provider publishes — not the
/// spelling the accessor is realized under and not the reference site's.
#[test]
fn a_dependency_property_reference_answers_its_name() {
    let source = "fun box(): String {\n\
         \x20   val reference = \"abc\"::length\n\
         \x20   return if (reference.name == \"length\") \"OK\" else \"fail \" + reference.name\n\
         }\n";
    every_backend_agrees_with_kotlinc("DependencyPropertyName", source);
}

/// Kotlin says two references to ONE declaration are equal, and that two bound to DIFFERENT
/// receivers are not.
#[test]
fn two_references_to_one_dependency_property_are_equal() {
    let source = "fun box(): String {\n\
         \x20   if (String::length != String::length) return \"fail unbound\"\n\
         \x20   val text = \"abc\"\n\
         \x20   if (text::length != text::length) return \"fail same receiver\"\n\
         \x20   if (text::length == \"abcd\"::length) return \"fail other receiver\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("DependencyPropertyEquality", source);
}

/// A reference that BINDS its receiver evaluates it once, where it is written.
#[test]
fn a_bound_dependency_property_reference_evaluates_its_receiver_once() {
    let source = "var evaluations = 0\n\
         fun receiver(): String {\n\
         \x20   evaluations++\n\
         \x20   return \"abc\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val length = receiver()::length\n\
         \x20   length.get()\n\
         \x20   length.get()\n\
         \x20   return if (evaluations == 1) \"OK\" else \"fail \" + evaluations\n\
         }\n";
    every_backend_agrees_with_kotlinc("DependencyPropertyReceiverOnce", source);
}
