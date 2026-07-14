//! An `inner class` captures its enclosing instance and may reference the OUTER class's type
//! parameters in its own member signatures, field/ctor-parameter types, and bodies (`inner class N`
//! using the outer `<T>`). Both signature collection and the member checker put the enclosing
//! class's type parameters (erased) in scope while resolving the inner class.

use crate::common;

#[test]
fn inner_class_reads_outer_type_param_in_member() {
    // The inner class's method return type and body reference the outer `<T>`.
    let src = r#"
class Box<T>(val value: T) {
    inner class Wrapper {
        fun get(): T = value
    }
    fun wrapper() = Wrapper()
}

fun box(): String {
    val b = Box("OK")
    return b.wrapper().get()
}
"#;
    common::expect_box_ok_with_stdlib(src, "InnerOuterTParam");
}

#[test]
fn inner_class_ctor_param_uses_outer_type_param() {
    // The inner class's constructor/field type references the outer `<T>`.
    let src = r#"
class Holder<T>(val seed: T) {
    inner class Cell(val extra: T) {
        fun pair(): String = "$seed$extra"
    }
}

fun box(): String {
    val h = Holder("O")
    return h.Cell("K").pair()
}
"#;
    common::expect_box_ok_with_stdlib(src, "InnerOuterCtorTParam");
}
