//! A SECONDARY constructor with default arguments.
//!
//! A primary constructor's defaults live on the CLASS, beside its parameters; a secondary's live
//! on the constructor. Both are filled by a wrapper that takes the operands actually supplied,
//! evaluates each missing default into the frame, and then runs the constructor — so what the
//! wrapper is keyed on has to name WHICH constructor it fills, not just which class and which
//! ordinals were left out. Two constructors of one class omitting the same ordinal are two
//! wrappers.
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

/// A secondary constructor whose only parameter is defaulted, called with nothing.
#[test]
fn a_secondary_constructor_fills_its_own_default() {
    let source = "class A {\n\
         \x20   val text: String\n\
         \x20   constructor(value: String = \"OK\") {\n\
         \x20       text = value\n\
         \x20   }\n\
         }\n\
         fun box(): String = A().text\n";
    every_backend_agrees_with_kotlinc("SecondaryDefault", source);
}

/// The default is only filled where the argument is ABSENT.
#[test]
fn a_supplied_argument_wins_over_the_default() {
    let source = "class A {\n\
         \x20   val text: String\n\
         \x20   constructor(value: String = \"fail\") {\n\
         \x20       text = value\n\
         \x20   }\n\
         }\n\
         fun box(): String = A(\"OK\").text\n";
    every_backend_agrees_with_kotlinc("SecondarySupplied", source);
}

/// Several parameters, with the omission in the MIDDLE — the wrapper fills the frame by ordinal
/// rather than by position among what was supplied.
#[test]
fn a_secondary_constructor_fills_a_middle_default() {
    let source = "class A {\n\
         \x20   val text: String\n\
         \x20   constructor(first: String, middle: String = \"K\", last: String) {\n\
         \x20       text = first + middle + last\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   val a = A(first = \"O\", last = \"!\")\n\
         \x20   return if (a.text == \"OK!\") \"OK\" else \"fail \" + a.text\n\
         }\n";
    every_backend_agrees_with_kotlinc("SecondaryMiddleDefault", source);
}

/// A default READING an earlier parameter, which is why the frame is filled in declaration order.
#[test]
fn a_default_may_read_an_earlier_parameter() {
    let source = "class A {\n\
         \x20   val text: String\n\
         \x20   constructor(head: String, tail: String = head + \"K\") {\n\
         \x20       text = tail\n\
         \x20   }\n\
         }\n\
         fun box(): String = A(\"O\").text\n";
    every_backend_agrees_with_kotlinc("SecondaryDefaultReadsEarlier", source);
}

/// The PRIMARY and a SECONDARY of one class, each with a default at the same ordinal: two
/// constructors, two frames, two wrappers.
#[test]
fn a_primary_and_a_secondary_keep_their_own_defaults() {
    let source = "class A(val text: String = \"primary\") {\n\
         \x20   constructor(marker: Int, extra: String = \"secondary\") : this(extra)\n\
         }\n\
         fun box(): String {\n\
         \x20   if (A().text != \"primary\") return \"fail primary\"\n\
         \x20   if (A(1).text != \"secondary\") return \"fail secondary\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("PrimaryAndSecondaryDefaults", source);
}

/// A SUPERCLASS's secondary constructor, reached by a subclass's delegation that omits an
/// argument — the same wrapper, named by a `super(…)` rather than by a construction.
#[test]
fn a_subclass_delegates_to_a_superclass_secondary_with_defaults() {
    let source = "open class B {\n\
         \x20   val text: String\n\
         \x20   constructor(value: String = \"OK\") {\n\
         \x20       text = value\n\
         \x20   }\n\
         }\n\
         class C : B()\n\
         fun box(): String = C().text\n";
    every_backend_agrees_with_kotlinc("SuperSecondaryDefault", source);
}

/// A secondary constructor DELEGATING to another secondary, the delegating one defaulting an
/// argument of its own. The arities differ, so which constructor `A("O", 1)` names is not in doubt.
#[test]
fn a_secondary_may_delegate_to_another_secondary() {
    let source = "class A {\n\
         \x20   val text: String\n\
         \x20   constructor(value: String) {\n\
         \x20       text = value\n\
         \x20   }\n\
         \x20   constructor(head: String, marker: Int, tail: String = \"K\") : this(head + tail)\n\
         }\n\
         fun box(): String = A(\"O\", 1).text\n";
    every_backend_agrees_with_kotlinc("SecondaryDelegatesToSecondary", source);
}
