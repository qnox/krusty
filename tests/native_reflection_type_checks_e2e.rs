//! `p is KProperty0<*>`, `p is KMutableProperty<*>` — an `is` check against one of Kotlin's
//! REFLECTION types.
//!
//! A property reference is an object of a type of its own: the generator emits one descriptor per
//! property, so the type written at the check site is never the object's. What the two have in
//! common is a runtime MARKER — `KCallable`, `KProperty`, `KPropertyN`, and `KMutableProperty` and
//! `KMutablePropertyN` beside them for a `var` — and every reference's descriptor names the whole
//! flattened chain, because an interface's own bases are not walked at an `is`.
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

/// A reference to a `var` wears the mutable half of the hierarchy as well as the read-only half.
#[test]
fn a_reference_to_a_var_is_a_mutable_property() {
    let source = "var counter = 1\n\
         fun box(): String {\n\
         \x20   val p: Any = ::counter\n\
         \x20   return if (p is kotlin.reflect.KMutableProperty<*>) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("VarIsMutableProperty", source);
}

/// A reference to a `val` does NOT, which is what makes the mutable marker worth carrying.
#[test]
fn a_reference_to_a_val_is_not_a_mutable_property() {
    let source = "val fixed = 1\n\
         fun box(): String {\n\
         \x20   val p: Any = ::fixed\n\
         \x20   return if (p is kotlin.reflect.KMutableProperty<*>) \"fail\" else \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ValIsNotMutableProperty", source);
}

/// Both wear the read-only half, and `KCallable` above it.
#[test]
fn every_property_reference_is_a_property_and_a_callable() {
    let source = "val fixed = 1\n\
         var counter = 2\n\
         fun box(): String {\n\
         \x20   val a: Any = ::fixed\n\
         \x20   val b: Any = ::counter\n\
         \x20   val ok = a is kotlin.reflect.KProperty<*> && b is kotlin.reflect.KProperty<*> &&\n\
         \x20       a is kotlin.reflect.KCallable<*> && b is kotlin.reflect.KCallable<*>\n\
         \x20   return if (ok) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("PropertyAndCallable", source);
}

/// ARITY separates the numbered markers: a top-level reference takes no receiver.
#[test]
fn a_top_level_property_reference_is_a_property0() {
    let source = "val fixed = 1\n\
         fun box(): String {\n\
         \x20   val p: Any = ::fixed\n\
         \x20   val ok = p is kotlin.reflect.KProperty0<*> && p !is kotlin.reflect.KProperty1<*, *>\n\
         \x20   return if (ok) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("TopLevelIsProperty0", source);
}

/// An UNBOUND member reference takes its receiver, so it is a `KProperty1`.
#[test]
fn an_unbound_member_reference_is_a_property1() {
    let source = "class Holder(val value: Int)\n\
         fun box(): String {\n\
         \x20   val p: Any = Holder::value\n\
         \x20   val ok = p is kotlin.reflect.KProperty1<*, *> && p !is kotlin.reflect.KProperty0<*>\n\
         \x20   return if (ok) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("UnboundMemberIsProperty1", source);
}

/// A BOUND one carries its receiver in the object, so it takes none and is a `KProperty0`.
#[test]
fn a_bound_member_reference_is_a_property0() {
    let source = "class Holder(val value: Int)\n\
         fun box(): String {\n\
         \x20   val p: Any = Holder(1)::value\n\
         \x20   val ok = p is kotlin.reflect.KProperty0<*> && p !is kotlin.reflect.KProperty1<*, *>\n\
         \x20   return if (ok) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("BoundMemberIsProperty0", source);
}

/// A value that is no reference at all answers false, so the markers are not on everything.
#[test]
fn a_value_that_is_no_reference_answers_false() {
    let source = "fun box(): String {\n\
         \x20   val s: Any = \"text\"\n\
         \x20   return if (s is kotlin.reflect.KProperty<*>) \"fail\" else \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("StringIsNoProperty", source);
}

/// The check does not disturb what the reference DOES: it still reads and writes.
#[test]
fn a_checked_reference_still_reads_and_writes() {
    let source = "var counter = 1\n\
         fun box(): String {\n\
         \x20   val p = ::counter\n\
         \x20   if (p !is kotlin.reflect.KMutableProperty<*>) return \"fail check\"\n\
         \x20   p.set(41)\n\
         \x20   return if (p.get() == 41) \"OK\" else \"fail \" + p.get()\n\
         }\n";
    every_backend_agrees_with_kotlinc("CheckedReferenceStillWorks", source);
}
