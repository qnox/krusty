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

/// A value that is no reference at all answers false, so the markers are not on everything.
#[test]
fn a_value_that_is_no_reference_answers_false() {
    let source = "fun box(): String {\n\
         \x20   val s: Any = \"text\"\n\
         \x20   return if (s is kotlin.reflect.KProperty<*>) \"fail\" else \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("StringIsNoProperty", source);
}
