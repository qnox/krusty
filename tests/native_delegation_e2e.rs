//! Interface delegation — `class C(d: I) : I by d` — through krusty's own code generator.
//!
//! The forwarder the compiler synthesizes on `C` calls the delegate through the STATIC type `I`,
//! which is the one shape that reaches the generator as `Callee::Virtual`: the receiver's own class
//! is unknown at the call, so the slot has to be the interface's program-wide number rather than
//! any implementation's. Every program here is a `box()` the native backend must LOWER — a decline
//! fails, which is the point of a directed test for a construct.

use super::common::expect_native_box;

#[test]
fn a_class_forwards_an_interface_member_to_its_delegate() {
    expect_native_box(
        "interface Named {\n\
         \x20   fun name(): String\n\
         }\n\
         class Fixed : Named {\n\
         \x20   override fun name() = \"OK\"\n\
         }\n\
         class Wrapper(delegate: Named) : Named by delegate\n\
         fun box(): String = Wrapper(Fixed()).name()\n",
        "DelegatedMember",
        "OK",
    );
}

#[test]
fn a_forwarded_member_carries_its_arguments_and_result() {
    expect_native_box(
        "interface Calculator {\n\
         \x20   fun add(left: Int, right: Int): Int\n\
         \x20   fun label(prefix: String, value: Int): String\n\
         }\n\
         class Adder : Calculator {\n\
         \x20   override fun add(left: Int, right: Int) = left + right\n\
         \x20   override fun label(prefix: String, value: Int) = prefix + value\n\
         }\n\
         class Wrapper(delegate: Calculator) : Calculator by delegate\n\
         fun box(): String {\n\
         \x20   val wrapper = Wrapper(Adder())\n\
         \x20   if (wrapper.add(2, 3) != 5) return \"fail: \" + wrapper.add(2, 3)\n\
         \x20   return if (wrapper.label(\"n=\", 5) == \"n=5\") \"OK\" else wrapper.label(\"n=\", 5)\n\
         }\n",
        "DelegatedArguments",
        "OK",
    );
}

#[test]
fn a_class_delegates_two_interfaces_to_two_delegates() {
    // Two interfaces numbered in one program-wide region: each forwarder must index the region
    // entry of the interface IT was written for, not the other's.
    expect_native_box(
        "interface First {\n\
         \x20   fun first(): String\n\
         }\n\
         interface Second {\n\
         \x20   fun second(): String\n\
         }\n\
         class A : First {\n\
         \x20   override fun first() = \"a\"\n\
         }\n\
         class B : Second {\n\
         \x20   override fun second() = \"b\"\n\
         }\n\
         class Both(a: First, b: Second) : First by a, Second by b\n\
         fun box(): String {\n\
         \x20   val both = Both(A(), B())\n\
         \x20   val joined = both.first() + both.second()\n\
         \x20   return if (joined == \"ab\") \"OK\" else joined\n\
         }\n",
        "DelegatedTwoInterfaces",
        "OK",
    );
}

#[test]
fn a_forwarder_reaches_the_delegates_own_override() {
    // The delegate is stored as `Named`, so the forwarder dispatches: the answer must come from the
    // runtime class, not from the interface's own default.
    expect_native_box(
        "interface Named {\n\
         \x20   fun name(): String = \"default\"\n\
         }\n\
         class Overriding : Named {\n\
         \x20   override fun name() = \"OK\"\n\
         }\n\
         class Wrapper(delegate: Named) : Named by delegate\n\
         fun box(): String = Wrapper(Overriding()).name()\n",
        "DelegatedOverride",
        "OK",
    );
}

#[test]
fn a_delegating_class_overrides_one_member_and_forwards_the_rest() {
    expect_native_box(
        "interface Pair {\n\
         \x20   fun left(): String\n\
         \x20   fun right(): String\n\
         }\n\
         class Both : Pair {\n\
         \x20   override fun left() = \"l\"\n\
         \x20   override fun right() = \"r\"\n\
         }\n\
         class Wrapper(delegate: Pair) : Pair by delegate {\n\
         \x20   override fun left() = \"L\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val wrapper = Wrapper(Both())\n\
         \x20   val joined = wrapper.left() + wrapper.right()\n\
         \x20   return if (joined == \"Lr\") \"OK\" else joined\n\
         }\n",
        "DelegatedPartialOverride",
        "OK",
    );
}

#[test]
fn a_forwarded_member_is_reached_through_the_interface_too() {
    // The delegating class seen AS the interface: the call site uses the interface's slot and the
    // forwarder fills it, so both spellings must land on the same entry.
    expect_native_box(
        "interface Named {\n\
         \x20   fun name(): String\n\
         }\n\
         class Fixed : Named {\n\
         \x20   override fun name() = \"OK\"\n\
         }\n\
         class Wrapper(delegate: Named) : Named by delegate\n\
         fun box(): String {\n\
         \x20   val named: Named = Wrapper(Fixed())\n\
         \x20   return named.name()\n\
         }\n",
        "DelegatedThroughInterface",
        "OK",
    );
}

#[test]
fn a_delegated_member_beats_a_redeclaring_interfaces_default() {
    // `Base2 : Base` redeclares `test` with a body, so the interface numbering meets the same
    // member under two spellings — `Base`'s and `Base2`'s. They are ONE number: give `Base2`'s a
    // second one and the class, which registered its forwarder under `Base`'s, appears to supply
    // nothing and silently takes `Base2`'s default instead of its delegate's answer.
    expect_native_box(
        "interface Base {\n\
         \x20   fun test() = \"base fail\"\n\
         }\n\
         interface Base2 : Base {\n\
         \x20   override fun test() = \"base 2fail\"\n\
         }\n\
         class Delegate : Base {\n\
         \x20   override fun test(): String = \"OK\"\n\
         }\n\
         class Impl : Base2, Base by Delegate()\n\
         fun box(): String = Impl().test()\n",
        "DelegatedOverRedeclaredDefault",
        "OK",
    );
}

#[test]
fn a_delegated_member_beats_a_redeclaring_interfaces_default_through_the_base() {
    // The same program called through the BASE interface: both spellings must index one entry.
    expect_native_box(
        "interface Base {\n\
         \x20   fun test() = \"base fail\"\n\
         }\n\
         interface Base2 : Base {\n\
         \x20   override fun test() = \"base 2fail\"\n\
         }\n\
         class Delegate : Base {\n\
         \x20   override fun test(): String = \"OK\"\n\
         }\n\
         class Impl : Base2, Base by Delegate()\n\
         fun box(): String {\n\
         \x20   val base: Base = Impl()\n\
         \x20   return base.test()\n\
         }\n",
        "DelegatedOverRedeclaredDefaultThroughBase",
        "OK",
    );
}

#[test]
fn an_anonymous_object_delegates_one_of_its_supertypes() {
    // `codegen/box/delegation/hiddenSuperOverrideIn1.0.kt`: the delegating classifier is the
    // anonymous object itself, which has no name for the forwarder to be found under.
    expect_native_box(
        "interface Base {\n\
         \x20   fun test() = \"base fail\"\n\
         }\n\
         interface Base2 : Base {\n\
         \x20   override fun test() = \"base 2fail\"\n\
         }\n\
         class Delegate : Base {\n\
         \x20   override fun test(): String = \"OK\"\n\
         }\n\
         fun box(): String = object : Base2, Base by Delegate() {}.test()\n",
        "DelegatedFromAnonymousObject",
        "OK",
    );
}

#[test]
fn an_interface_overriding_a_member_two_bases_declare_keeps_both_numbers() {
    // `A.name` and `B.name` are two members of the program that `Both` fills with one entry: an
    // implementor has to answer through EITHER base's spelling, so neither number may be dropped
    // when the two are met at one slot.
    expect_native_box(
        "interface A {\n\
         \x20   fun name(): String\n\
         }\n\
         interface B {\n\
         \x20   fun name(): String\n\
         }\n\
         interface Both : A, B {\n\
         \x20   override fun name() = \"OK\"\n\
         }\n\
         class Impl : Both\n\
         fun box(): String {\n\
         \x20   val impl = Impl()\n\
         \x20   val a: A = impl\n\
         \x20   val b: B = impl\n\
         \x20   if (a.name() != \"OK\") return \"fail A: \" + a.name()\n\
         \x20   if (b.name() != \"OK\") return \"fail B: \" + b.name()\n\
         \x20   return impl.name()\n\
         }\n",
        "DiamondOverride",
        "OK",
    );
}
