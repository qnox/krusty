//! What a VIRTUAL call yields, when the member it dispatches to is generic.
//!
//! A virtual call goes through a slot, and the slot carries the member's DECLARED return rather
//! than the one this receiver's class narrows it to. For a generic member that declared type is a
//! type parameter, so the value is a reference — and saying so is the whole of what lets a site
//! wanting a machine value unbox it. Left undetermined, such a value reached an `Int` position as
//! a reference with nothing to convert it by.
//!
//! `class A(a: Tr<Int>) : Tr<Int> by a` is the shape that makes this visible: the delegation
//! forwards through the interface's slot, so the boxed value crosses even though both ends of the
//! program say `Int`.
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

/// Require kotlinc's answer and the NATIVE answer to agree, leaving krusty's JVM backend out.
///
/// For a delegated generic PROPERTY only, where that backend has a gap of its own: it emits no
/// implementation of the interface's accessor on the delegating class, so the program dies with
/// `AbstractMethodError: Receiver class A does not define or inherit ... getProp()`. kotlinc runs
/// the same source correctly, so the expectation below is still kotlinc's; what is left out is a
/// second backend that cannot yet meet it. A delegated generic FUNCTION has no such gap and is
/// checked against all three.
fn kotlinc_and_native_agree(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_native_box(source, stem, "OK");
}

/// A delegated generic PROPERTY, read into a position that wants a machine `Int`.
#[test]
fn a_delegated_generic_property_unboxes_where_an_int_is_wanted() {
    let source = "interface Tr<T> {\n\
         \x20   val prop: T\n\
         }\n\
         class A(a: Tr<Int>) : Tr<Int> by a\n\
         fun eat(x: Int): Int = x\n\
         fun box(): String {\n\
         \x20   val held = A(object : Tr<Int> {\n\
         \x20       override val prop = 42\n\
         \x20   })\n\
         \x20   return if (eat(held.prop) == 42) \"OK\" else \"fail \" + eat(held.prop)\n\
         }\n";
    kotlinc_and_native_agree("DelegatedGenericProperty", source);
}

/// A delegated generic FUNCTION, whose argument and result both cross the slot.
#[test]
fn a_delegated_generic_function_unboxes_its_result() {
    let source = "interface Base<T> {\n\
         \x20   fun foo(a: T): T\n\
         }\n\
         class BaseImpl(val a: Base<Int>) : Base<Int> by a\n\
         fun eat(x: Int): Int = x\n\
         fun box(): String {\n\
         \x20   val b = BaseImpl(object : Base<Int> {\n\
         \x20       override fun foo(a: Int): Int = a + 1\n\
         \x20   })\n\
         \x20   return if (eat(b.foo(41)) == 42) \"OK\" else \"fail \" + b.foo(41)\n\
         }\n";
    every_backend_agrees_with_kotlinc("DelegatedGenericFunction", source);
}

/// Through the INTERFACE's type rather than the class's, which is the same slot either way.
#[test]
fn the_same_answer_comes_through_the_interface_type() {
    let source = "interface Tr<T> {\n\
         \x20   val prop: T\n\
         }\n\
         class A(a: Tr<Int>) : Tr<Int> by a\n\
         fun eat(x: Int): Int = x\n\
         fun box(): String {\n\
         \x20   val held: Tr<Int> = A(object : Tr<Int> {\n\
         \x20       override val prop = 7\n\
         \x20   })\n\
         \x20   return if (eat(held.prop) == 7) \"OK\" else \"fail\"\n\
         }\n";
    kotlinc_and_native_agree("DelegatedThroughInterface", source);
}

/// A generic member substituted to a REFERENCE needs no conversion, and still answers.
#[test]
fn a_generic_member_substituted_to_a_reference_answers_too() {
    let source = "interface Tr<T> {\n\
         \x20   val prop: T\n\
         }\n\
         class A(a: Tr<String>) : Tr<String> by a\n\
         fun box(): String {\n\
         \x20   val held = A(object : Tr<String> {\n\
         \x20       override val prop = \"OK\"\n\
         \x20   })\n\
         \x20   return held.prop\n\
         }\n";
    kotlinc_and_native_agree("DelegatedGenericReference", source);
}
