//! Synthetic accessors for `private` members reached from a SEPARATE class in the same compilation.
//! A callable reference (`::priv`), a bound qualified-this reference inside a lambda, and an inner
//! class calling the outer's private method all require the private member to be reachable without an
//! illegal `invokespecial`/private access — kotlinc synthesizes `access$m`; krusty forwards through a
//! public `access$m` instance method. These compile to `ACC_PRIVATE` members plus accessors and RUN.

use super::common;

#[test]
fn callable_reference_to_private_member() {
    // `::priv` inside the class → a func-ref class must reach the private method via the accessor.
    common::assert_box_ok_with_stdlib(
        "class A {\n  private fun foo(): String = \"OK\"\n  fun r(): () -> String = ::foo\n}\n\
         fun box(): String = A().r()()\n",
        "Cref",
    );
}

#[test]
fn bound_reference_to_private_in_lambda() {
    // A bound `this@A::priv` captured inside a lambda body (kotlinc KT-63258 shape).
    common::assert_box_ok_with_stdlib(
        "class A {\n  private val ref: () -> String = run { this@A::foo }\n  \
         private fun foo(): String = \"OK\"\n  fun r(): String = ref()\n}\n\
         fun box(): String = A().r()\n",
        "Bound",
    );
}

#[test]
fn inner_class_calls_outer_private() {
    // An inner class directly calling the outer's private method → routed through the accessor.
    common::assert_box_ok_with_stdlib(
        "class Outer {\n  private fun secret(): String = \"OK\"\n  \
         inner class Inner {\n    fun get(): String = secret()\n  }\n}\n\
         fun box(): String = Outer().Inner().get()\n",
        "Inner",
    );
}
