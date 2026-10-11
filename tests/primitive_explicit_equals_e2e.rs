//! An explicit `equals` call on a primitive receiver compares two objects through `Object.equals`.
//!
//! kotlinc's `ExplicitEquals` intrinsic realizes every builtin scalar's `equals(Any?)`: it boxes
//! the receiver (and a primitive argument) and calls `java/lang/Object.equals`, never the wrapper's
//! own `equals`. A class receiver keeps its ordinary dispatch on its own class.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      class Plain\n\
                      open class Custom { override fun equals(other: Any?): Boolean = true }\n\
                      class Derived : Custom()\n\
                      \n\
                      fun ints(a: Int, b: Any?) = a.equals(b)\n\
                      fun shorts(a: Short, b: Short?) = a.equals(b)\n\
                      fun chars(a: Char, b: Any?) = a.equals(b)\n\
                      fun flags(a: Boolean, b: Any?) = a.equals(b)\n\
                      fun doubles(a: Double, b: Any?) = a.equals(b)\n\
                      fun widened(a: Int, b: Byte) = a.equals(b)\n\
                      fun text(a: String, b: Any?) = a.equals(b)\n\
                      fun plain(a: Plain, b: Any?) = a.equals(b)\n\
                      fun custom(a: Custom, b: Any?) = a.equals(b)\n\
                      fun derived(a: Derived, b: Any?) = a.equals(b)\n";

#[test]
fn explicit_primitive_equals_calls_object_equals_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "PrimitiveExplicitEquals",
        SOURCE,
        "store/PrimitiveExplicitEqualsKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/PrimitiveExplicitEqualsKt differs from kotlinc: {diff}"));
}
