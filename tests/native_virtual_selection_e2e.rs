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
