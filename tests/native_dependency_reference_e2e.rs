//! `"KOTLIN"::get`, `Boolean::not`, `String::plus` — a callable reference to a DEPENDENCY
//! declaration, whose site type is a reflective `KFunctionN`.
//!
//! Common lowering builds such a reference into an adapter when the site's type is a plain
//! `FunctionN`, and leaves it CHECKED when the type is reflective, so that each target may choose
//! its own reflection representation. The native backend's choice is the same adapter, with the
//! declaration's identity kept beside it — which is what makes two references to one declaration
//! equal and `name` answer the declaration's own name.
//!
//! Every expectation here is kotlinc's, taken by running the same program under it.

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

/// A BOUND reference to a dependency member, whose receiver the value carries.
#[test]
fn a_bound_reference_to_a_dependency_member_calls_it() {
    let source = "fun box(): String {\n\
         \x20   val f = \"KOTLIN\"::get\n\
         \x20   return \"${f(1)}${f(0)}\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("BoundDependencyReference", source);
}

/// A reference with an ARGUMENT beyond the receiver, and one whose declaration is answered by a
/// runtime entry point rather than by an instruction.
#[test]
fn a_reference_carrying_an_argument_passes_it() {
    let source = "fun box(): String {\n\
         \x20   val plus = String::plus\n\
         \x20   if (plus(\"O\", \"K\") != \"OK\") return \"fail unbound\"\n\
         \x20   val bound = \"O\"::plus\n\
         \x20   return if (bound(\"K\") == \"OK\") \"OK\" else \"fail bound\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("DependencyReferenceArgument", source);
}

/// Kotlin says two references to ONE declaration are equal though they are different objects, and
/// that a reference with a receiver bound to it is not equal to the unbound one.
#[test]
fn two_references_to_one_dependency_declaration_are_equal() {
    let source = "fun box(): String {\n\
         \x20   val one = String::plus\n\
         \x20   val other = String::plus\n\
         \x20   if (one != other) return \"fail unbound equality\"\n\
         \x20   if (one.hashCode() != other.hashCode()) return \"fail unbound hash\"\n\
         \x20   if (one == \"O\"::plus) return \"fail bound against unbound\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("DependencyReferenceEquality", source);
}

/// `KCallable.name` answers the declaration's own name, which the provider publishes beside the
/// spelling the declaration is realized under — and a program reads it through a VARIABLE, so the
/// object answers it rather than the site folding it.
#[test]
fn a_dependency_reference_answers_its_declarations_name() {
    let source = "fun box(): String {\n\
         \x20   val f = \"KOTLIN\"::get\n\
         \x20   return if (f.name == \"get\") \"OK\" else \"fail \" + f.name\n\
         }\n";
    every_backend_agrees_with_kotlinc("DependencyReferenceName", source);
}

/// A reference to a declaration of THIS FILE answers its name the same way, through the same slot.
#[test]
fn a_module_reference_answers_its_name_through_a_variable() {
    let source = "fun target(value: Int): Int = value\n\
         fun box(): String {\n\
         \x20   val f = ::target\n\
         \x20   if (f(1) != 1) return \"fail call\"\n\
         \x20   return if (f.name == \"target\") \"OK\" else \"fail \" + f.name\n\
         }\n";
    every_backend_agrees_with_kotlinc("ModuleReferenceName", source);
}

/// A reference to a member a SOURCE FORM is spelled as — `!b`, which the frontend supplies as a
/// compiler operation for the form and as an ordinary dependency call for the reference. The
/// declaration is the same, so the answer has to be.
#[test]
fn a_reference_to_a_member_a_source_form_spells_calls_it() {
    let source = "fun box(): String {\n\
         \x20   if ((Boolean::not).let { it(true) } != false) return \"fail not true\"\n\
         \x20   if ((Boolean::not).let { it(false) } != true) return \"fail not false\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("PrimitiveMemberReference", source);
}

/// A reference that BINDS its receiver evaluates that receiver once, where it is written — not
/// again at each call.
#[test]
fn a_bound_dependency_reference_evaluates_its_receiver_once() {
    let source = "var evaluations = 0\n\
         fun receiver(): String {\n\
         \x20   evaluations++\n\
         \x20   return \"OK\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val f = receiver()::get\n\
         \x20   f(0)\n\
         \x20   f(1)\n\
         \x20   return if (evaluations == 1) \"OK\" else \"fail \" + evaluations\n\
         }\n";
    every_backend_agrees_with_kotlinc("DependencyReferenceReceiverOnce", source);
}
