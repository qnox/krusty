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

#[test]
fn inner_class_ctor_param_resolves_a_sibling_inner_class() {
    let src = r#"
class Outer {
    inner class First(val value: String)

    inner class Second(val first: First) {
        fun read(): String = first.value
    }

    fun make(): String = Second(First("OK")).read()
}

fun box(): String = Outer().make()
"#;

    common::expect_box_ok_with_stdlib(src, "InnerSiblingCtorType");
}

#[test]
fn nearest_lexical_owner_wins_for_a_sibling_ctor_type() {
    let src = r#"
class Outer {
    class Value(val text: String)

    class Middle {
        class Value(val text: String)
        class Use(val value: Value)

        fun make(): String = Use(Value("OK")).value.text
    }
}

fun box(): String = Outer.Middle().make()
"#;

    common::expect_box_ok_with_stdlib(src, "NearestSiblingCtorType");
}

#[test]
fn shadowed_inner_parameter_does_not_erase_the_outer_parameter_identity() {
    let src = r#"
class Outer<T>(val outer: T) {
    inner class Inner<T>(val inner: T) {
        fun result(): String = this@Outer.outer.toString() + inner.toString()
    }
}

fun box(): String = Outer("O").Inner("K").result()
"#;

    common::expect_box_ok_with_stdlib(src, "ShadowedInnerOuterTypeParameter");
}

#[test]
fn three_level_inner_class_carries_every_enclosing_type_argument() {
    let src = r#"
class Outer<T>(val t: T) {
    inner class Middle<U>(val u: U) {
        inner class Inner<V>(val v: V) {
            fun result(): String = t.toString() + u.toString() + v.toString()
        }
    }
}

fun box(): String {
    val outer = Outer("O")
    val middle = outer.Middle("K")
    return middle.Inner("").result()
}
"#;

    common::expect_box_ok_with_stdlib(src, "ThreeLevelInnerTypeParameters");
}
