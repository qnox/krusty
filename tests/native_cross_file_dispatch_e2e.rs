//! A call dispatched through a class another file of the module declares.
//!
//! The calling file has no IR of the class, so the slot it indexes comes from the module's slot
//! plan, which numbers every named class from the declaration headers alone; the declaring file
//! fills the same numbers from its IR. Each program is first run on the JVM, then on Native.

use crate::common::{expect_box_ok_files_with_stdlib, expect_native_sources};

/// A portable multi-file program: the JVM answer is established first, then Native must agree.
fn expect_portable_sources(sources: &[(&str, &str)]) {
    let stem = sources.first().map(|(stem, _)| *stem).unwrap_or("module");
    expect_box_ok_files_with_stdlib(sources, stem);
    expect_native_sources(sources, "OK");
}

#[test]
fn an_open_member_dispatches_to_the_override_from_another_file() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                open class Base {
                    open fun name(): String = "base"
                }
                class Derived : Base() {
                    override fun name(): String = "derived"
                }
                fun make(derived: Boolean): Base = if (derived) Derived() else Base()
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String =
                    if (make(true).name() == "derived" && make(false).name() == "base") "OK"
                    else "FAIL"
            "#,
        ),
    ]);
}

#[test]
fn an_abstract_member_dispatches_to_its_implementation() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                abstract class Shape {
                    abstract fun area(scale: Int): Int
                }
                class Square(val side: Int) : Shape() {
                    override fun area(scale: Int): Int = side * side * scale
                }
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun measure(shape: Shape): Int = shape.area(2)
                fun box(): String = if (measure(Square(3)) == 18) "OK" else "FAIL"
            "#,
        ),
    ]);
}

#[test]
fn an_inherited_member_dispatches_through_a_subclass_type() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                open class Base {
                    fun greet(): String = "hi " + who()
                    open fun who(): String = "base"
                }
                open class Middle : Base()
                class Leaf : Middle() {
                    override fun who(): String = "leaf"
                }
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val middle: Middle = Leaf()
                    return if (middle.greet() == "hi leaf" && middle.who() == "leaf") "OK"
                    else "FAIL"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_generic_member_reaches_an_override_through_its_bridge() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                open class Holder<T>(private val initial: T) {
                    open fun get(): T = initial
                }
                class IntHolder(private val n: Int) : Holder<Int>(0) {
                    override fun get(): Int = n + 1
                }
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val holder: Holder<Int> = IntHolder(41)
                    return if (holder.get() == 42) "OK" else "FAIL"
                }
            "#,
        ),
    ]);
}

#[test]
fn an_overridden_to_string_dispatches_through_the_base() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                open class Named {
                    override fun toString(): String = "named"
                }
                class Special : Named() {
                    override fun toString(): String = "special"
                }
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val named: Named = Special()
                    return if (named.toString() == "special") "OK" else "FAIL"
                }
            "#,
        ),
    ]);
}

#[test]
fn an_open_property_dispatches_to_its_override() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                open class Base {
                    open val label: String = "base"
                    open var count: Int = 0
                }
                class Derived : Base() {
                    override val label: String get() = "derived"
                    override var count: Int = 10
                        set(value) { field = value * 2 }
                }
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val base: Base = Derived()
                    base.count = 21
                    return if (base.label == "derived" && base.count == 42) "OK" else "FAIL"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_delegated_member_replaces_the_inherited_class_member() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                interface A {
                    var x: String
                }
                open class B : A {
                    override lateinit var x: String
                }
                interface C : A
                open class D : C {
                    override var x: String
                        get() = "OK"
                        set(_) {}
                }
                class E : B(), C by D()
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val e = E()
                    e.x = "Fail"
                    val b: B = e
                    return if (e.x == "OK" && b.x == "OK") "OK" else "FAIL"
                }
            "#,
        ),
    ]);
}
