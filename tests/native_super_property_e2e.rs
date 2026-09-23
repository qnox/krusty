//! `super<B>.p` where B does not DECLARE `p`.
//!
//! `super` names a realization, not a declaration: an `open val` on an interface that B merely
//! inherits is still what `super<B>.p` reads, and `super<C2>.p` reads the `p` that C2's own
//! superclass declares. The search for it therefore walks up from the named class — its superclass
//! chain and the interfaces met on the way — and answers the first declaration it meets, which is
//! the one B's realization reaches. Searching the named class alone found nothing and declined.
//!
//! A property is what makes this show at all: `super.f()` on a METHOD finds a method to call, but a
//! class whose accessors are the default ones declares no method for its property, so the accessor
//! search comes up empty and the property search is what answers.
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

/// An `open val` an INTERFACE declares, read through a class that only inherits it.
#[test]
fn a_super_read_reaches_an_interfaces_property() {
    let source = r#"
interface A {
    val foo: String get() = "OK"
}

open class B : A

class C : B() {
    inner class D {
        val foo: String = super<B>@C.foo
    }
}

fun box(): String = C().D().foo
"#;
    every_backend_agrees_with_kotlinc("native_super_interface_property", source);
}

/// A `var` a SUPERCLASS declares, read and written through a class in between that only inherits
/// it — so both accessors have to be found the same way.
#[test]
fn a_super_write_reaches_an_inherited_property() {
    let source = r#"
open class C {
    open var p2 = "<C>"
        set(value) { field = "<C>" + value }
}

open class Middle : C()

class Leaf : Middle() {
    override var p2 = super<Middle>.p2 + "<leaf>"
        set(value) {
            super<Middle>.p2 = value
            field = "<leaf>" + super<Middle>.p2
        }
}

fun box(): String {
    val leaf = Leaf()
    leaf.p2 = "zzz"
    if (leaf.p2 != "<leaf><C>zzz") return "fail " + leaf.p2
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_super_inherited_property", source);
}

/// Two steps up, and through an interface a deeper base brought in.
#[test]
fn a_super_read_walks_more_than_one_step() {
    let source = r#"
interface Deep {
    val deepProp: String get() = "deep"
}

interface Middle : Deep

open class Base : Middle

open class Derived : Base()

class Leaf : Derived() {
    fun read(): String = super.deepProp
}

fun box(): String = if (Leaf().read() == "deep") "OK" else "fail " + Leaf().read()
"#;
    every_backend_agrees_with_kotlinc("native_super_deep_property", source);
}
