//! A virtual call reaches the member the checker selected, found by its declaration rather than
//! by its name.
//!
//! Each program runs under the JVM first, so the expectation is the one kotlinc's semantics give,
//! and the Native answer must then agree.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

fn expect_portable(source: &str, stem: &str) {
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

#[test]
fn an_overload_dispatches_to_the_member_the_call_selected() {
    expect_portable(
        "open class A {\n\
         \x20   open fun f(x: Int): String = \"i\" + x\n\
         \x20   open fun f(x: String): String = \"s\" + x\n\
         \x20   open fun f(x: Int, y: Int): String = \"ii\" + (x + y)\n\
         }\n\
         class B : A() {\n\
         \x20   override fun f(x: String): String = \"S\" + x\n\
         \x20   override fun f(x: Int, y: Int): String = \"II\" + (x * y)\n\
         }\n\
         fun box(): String {\n\
         \x20   val a: A = B()\n\
         \x20   val result = a.f(1) + a.f(\"x\") + a.f(2, 3)\n\
         \x20   return if (result == \"i1SxII6\") \"OK\" else result\n\
         }\n",
        "OverloadSelection",
    );
}

#[test]
fn an_inherited_member_is_dispatched_through_a_subclass_type() {
    expect_portable(
        "open class A {\n\
         \x20   open fun g(): String = \"A\"\n\
         \x20   fun h(): String = \"h\" + g()\n\
         }\n\
         open class B : A()\n\
         class C : B() {\n\
         \x20   override fun g(): String = \"C\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val b: B = C()\n\
         \x20   val result = b.g() + b.h() + B().g()\n\
         \x20   return if (result == \"ChCA\") \"OK\" else result\n\
         }\n",
        "InheritedSelection",
    );
}

/// The forwarders of `I by d` read and write the delegate's property through a virtual call that
/// names the interface property's getter and setter, so this is the producer of those targets.
#[test]
fn a_delegated_property_reaches_the_delegate_through_its_accessors() {
    expect_portable(
        "interface Box {\n\
         \x20   var value: String\n\
         \x20   val size: Int\n\
         }\n\
         interface Labeled : Box\n\
         class Cell : Labeled {\n\
         \x20   override var value: String = \"\"\n\
         \x20       set(v) { field = v + \"!\" }\n\
         \x20   override val size: Int get() = value.length\n\
         }\n\
         open class Wrapper(inner: Labeled) : Labeled by inner\n\
         class Leaf(inner: Labeled) : Wrapper(inner)\n\
         fun box(): String {\n\
         \x20   val leaf: Wrapper = Leaf(Cell())\n\
         \x20   leaf.value = \"OK\"\n\
         \x20   val result = leaf.value + leaf.size\n\
         \x20   return if (result == \"OK!3\") \"OK\" else result\n\
         }\n",
        "DelegatedPropertySelection",
    );
}
