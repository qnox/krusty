//! An interface member this file's class implements, which no OVERRIDE TABLE names.
//!
//! Two shapes leave an interface's number empty while the method that fills it is already in the
//! class's own vtable. A member EXTENSION's override is recorded in no override table — the
//! frontend keeps none for one — so nothing aliases the interface's spelling onto the override's
//! slot. And a member an `Interface by delegate` clause supplies is recorded against the one
//! interface it forwards to, so a SECOND interface declaring the same member is left empty
//! although the delegation answers it too.
//!
//! The native model reads the number by signature in both cases, which is the rule it already
//! uses up the superclass chain. Every expectation is kotlinc's, taken by running the same
//! program under it.

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

/// `override fun Int.foo()` of an interface's member extension, called through the class.
#[test]
fn a_member_extension_declared_in_an_interface_reaches_its_override() {
    let source = "interface Base {\n\
         \x20   fun Int.foo(): String\n\
         }\n\
         class A : Base {\n\
         \x20   override fun Int.foo(): String = \"OK\"\n\
         }\n\
         fun box(): String { with(A()) { return 1.foo() } }\n";
    every_backend_agrees_with_kotlinc("InterfaceMemberExtension", source);
}

/// The same for a member extension PROPERTY, whose accessor reaches the region as a method.
#[test]
fn a_member_extension_property_declared_in_an_interface_reaches_its_override() {
    let source = "interface Base {\n\
         \x20   val Int.a: String\n\
         }\n\
         class A : Base {\n\
         \x20   override val Int.a: String get() = \"OK\"\n\
         }\n\
         fun box(): String { with(A()) { return 1.a } }\n";
    every_backend_agrees_with_kotlinc("InterfaceMemberExtensionProperty", source);
}

/// Both halves at once, which is what the corpus case does.
#[test]
fn an_interface_declaring_a_member_extension_and_a_property_reaches_both() {
    let source = "interface Base {\n\
         \x20   fun Int.foo(): String\n\
         \x20   val Int.a: String\n\
         }\n\
         class A : Base {\n\
         \x20   override fun Int.foo(): String = \"O\"\n\
         \x20   override val Int.a: String get() = \"K\"\n\
         }\n\
         fun box(): String { with(A()) { return 1.foo() + 1.a } }\n";
    every_backend_agrees_with_kotlinc("InterfaceMemberExtensionBoth", source);
}

/// A DELEGATION answers a second interface that declares the same member. `Z` implements `B`
/// and supplies nothing of its own: `A by a` is what fills `B.foo` too.
#[test]
fn a_delegation_fills_a_second_interface_declaring_the_same_member() {
    let source = "interface A { fun foo(): Int }\n\
         interface B { fun foo(): Int }\n\
         class Z(val a: A) : A by a, B\n\
         fun box(): String {\n\
         \x20   val s = Z(object : A { override fun foo(): Int = 1 })\n\
         \x20   return if (s.foo() == 1) \"OK\" else \"fail \" + s.foo()\n\
         }\n";
    every_backend_agrees_with_kotlinc("DelegationFillsSecondInterface", source);
}

/// Reached through the INTERFACE's type rather than the class's, so the number is what answers.
#[test]
fn the_number_a_second_interface_owns_dispatches_to_the_delegate() {
    let source = "interface A { fun foo(): Int }\n\
         interface B { fun foo(): Int }\n\
         class Z(val a: A) : A by a, B\n\
         fun box(): String {\n\
         \x20   val b: B = Z(object : A { override fun foo(): Int = 1 })\n\
         \x20   return if (b.foo() == 1) \"OK\" else \"fail \" + b.foo()\n\
         }\n";
    every_backend_agrees_with_kotlinc("DelegationThroughSecondInterface", source);
}

/// An implementation a BASE CLASS supplies fills the number too — the search walks the chain.
/// `A` declares nothing: what answers `Base.foo` through `A` is the override `Mid` made.
#[test]
fn a_superclass_implementation_fills_the_interface_number_of_a_subclass() {
    let source = "interface Base {\n\
         \x20   fun Int.foo(): String\n\
         }\n\
         open class Mid : Base {\n\
         \x20   override fun Int.foo(): String = \"OK\"\n\
         }\n\
         class A : Mid()\n\
         fun box(): String { with(A()) { return 1.foo() } }\n";
    every_backend_agrees_with_kotlinc("SuperclassFillsInterfaceMember", source);
}

/// A SUBCLASS overriding what the base supplied is what the number answers with, not the base's.
#[test]
fn an_override_below_the_class_that_filled_the_number_is_what_answers() {
    let source = "interface Base {\n\
         \x20   fun Int.foo(): String\n\
         }\n\
         open class Mid : Base {\n\
         \x20   override fun Int.foo(): String = \"base\"\n\
         }\n\
         class A : Mid() {\n\
         \x20   override fun Int.foo(): String = \"OK\"\n\
         }\n\
         fun box(): String { with(A()) { return 1.foo() } }\n";
    every_backend_agrees_with_kotlinc("SubclassOverridesFilledNumber", source);
}
