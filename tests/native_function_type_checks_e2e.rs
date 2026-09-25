//! `x is Function0<*>`, `x is Function<*>` — an `is` check against a FUNCTION TYPE.
//!
//! A function value is an object of a type of its own: the generator emits one descriptor per
//! lambda and per callable reference, so the type written at the check site is never the object's.
//! What the two have in common is a runtime MARKER, one per arity plus the bare `kotlin.Function`,
//! which every function value's descriptor names.
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

/// A lambda of the arity asked about.
#[test]
fn a_lambda_is_a_function_of_its_own_arity() {
    let source = "fun box(): String {\n\
         \x20   val f: Any = { 1 }\n\
         \x20   return if (f is Function0<*>) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("LambdaIsFunction0", source);
}

/// ARITY is what separates the markers: a `Function1` is no `Function0`.
#[test]
fn a_lambda_is_not_a_function_of_another_arity() {
    let source = "fun box(): String {\n\
         \x20   val f: Any = { x: Int -> x }\n\
         \x20   return if (f is Function0<*>) \"fail\" else \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("LambdaIsNotFunction0", source);
}

/// The one-argument lambda answers its OWN arity.
#[test]
fn a_one_argument_lambda_is_a_function1() {
    let source = "fun box(): String {\n\
         \x20   val f: Any = { x: Int -> x }\n\
         \x20   return if (f is Function1<*, *>) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("LambdaIsFunction1", source);
}

/// The bare `kotlin.Function` is named beside the arity, because the interface list is flattened.
#[test]
fn a_lambda_of_any_arity_is_a_function() {
    let source = "fun box(): String {\n\
         \x20   val zero: Any = { 1 }\n\
         \x20   val two: Any = { a: Int, b: Int -> a + b }\n\
         \x20   return if (zero is Function<*> && two is Function<*>) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("LambdaIsFunction", source);
}

/// A value that is no function at all answers false, so the marker is not on everything.
#[test]
fn a_value_that_is_no_function_answers_false() {
    let source = "fun box(): String {\n\
         \x20   val s: Any = \"text\"\n\
         \x20   return if (s is Function<*> || s is Function0<*>) \"fail\" else \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("StringIsNoFunction", source);
}

/// A CALLABLE REFERENCE is a function value too, and wears the same markers.
#[test]
fn a_callable_reference_is_a_function_of_its_arity() {
    let source = "fun target(x: Int): Int = x\n\
         fun box(): String {\n\
         \x20   val f: Any = ::target\n\
         \x20   return if (f is Function1<*, *> && f is Function<*>) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ReferenceIsFunction1", source);
}

/// `as` reads the same answer the check does, so a cast to the right type goes through and the
/// value is still callable after it.
#[test]
fn a_cast_to_a_function_type_keeps_the_value_callable() {
    let source = "fun box(): String {\n\
         \x20   val f: Any = { x: Int -> x + 1 }\n\
         \x20   @Suppress(\"UNCHECKED_CAST\")\n\
         \x20   val g = f as Function1<Int, Int>\n\
         \x20   return if (g.invoke(1) == 2) \"OK\" else \"fail \" + g.invoke(1)\n\
         }\n";
    every_backend_agrees_with_kotlinc("CastToFunction1", source);
}
