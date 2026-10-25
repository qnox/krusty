//! A call through a module interface goes to the member's stub, which the final link writes from
//! every type of the program: a direct jump for a member one function implements, a type
//! comparison for one two functions implement, and the interface table lookup otherwise. The
//! programs below take each shape, including the ones where a wrong choice would call the wrong
//! body. Each program is first run on the JVM, then on Native.

use crate::common::{expect_box_ok_files_with_stdlib, expect_native_sources};

/// A portable multi-file program: the JVM answer is established first, then Native must agree.
fn expect_portable_sources(sources: &[(&str, &str)]) {
    let stem = sources.first().map(|(stem, _)| *stem).unwrap_or("module");
    expect_box_ok_files_with_stdlib(sources, stem);
    expect_native_sources(sources, "OK");
}

#[test]
fn a_member_one_function_implements_is_called_directly() {
    expect_portable_sources(&[(
        "box",
        r#"
            package demo
            interface Shape { fun area(scale: Int): Int }
            class Square(private val side: Int) : Shape {
                override fun area(scale: Int): Int = side * side * scale
            }
            fun measure(shape: Shape): Int = shape.area(2)
            fun box(): String = if (measure(Square(3)) == 18) "OK" else "FAIL"
        "#,
    )]);
}

#[test]
fn a_member_two_functions_implement_tells_the_types_apart() {
    expect_portable_sources(&[(
        "box",
        r#"
            package demo
            interface Shape { fun area(): Int }
            class Square(private val side: Int) : Shape { override fun area(): Int = side * side }
            class Rectangle(private val w: Int, private val h: Int) : Shape {
                override fun area(): Int = w * h
            }
            fun total(shapes: List<Shape>): Int {
                var sum = 0
                for (shape in shapes) sum += shape.area()
                return sum
            }
            fun box(): String {
                val shapes = listOf(Square(3), Rectangle(2, 5), Square(1), Rectangle(1, 1))
                return if (total(shapes) == 21) "OK" else "FAIL"
            }
        "#,
    )]);
}

#[test]
fn a_member_three_functions_implement_reads_the_interface_table() {
    expect_portable_sources(&[(
        "box",
        r#"
            package demo
            interface Shape { fun name(): String }
            class Square : Shape { override fun name(): String = "square" }
            class Circle : Shape { override fun name(): String = "circle" }
            class Triangle : Shape { override fun name(): String = "triangle" }
            fun names(shapes: List<Shape>): String {
                var all = ""
                for (shape in shapes) all = if (all.isEmpty()) shape.name() else all + "," + shape.name()
                return all
            }
            fun box(): String {
                val all = names(listOf(Triangle(), Square(), Circle(), Square()))
                return if (all == "triangle,square,circle,square") "OK" else all
            }
        "#,
    )]);
}

#[test]
fn implementations_in_other_files_are_counted_together() {
    expect_portable_sources(&[
        (
            "box",
            r#"
                package demo
                fun describe(animal: Animal): String = animal.sound() + animal.legs()
                fun box(): String {
                    val heard = describe(Dog()) + describe(Bird())
                    return if (heard == "woof4tweet2") "OK" else heard
                }
            "#,
        ),
        (
            "animal",
            r#"
                package demo
                interface Animal {
                    fun sound(): String
                    fun legs(): Int
                }
            "#,
        ),
        (
            "dog",
            r#"
                package demo
                class Dog : Animal {
                    override fun sound(): String = "woof"
                    override fun legs(): Int = 4
                }
            "#,
        ),
        (
            "bird",
            r#"
                package demo
                class Bird : Animal {
                    override fun sound(): String = "tweet"
                    override fun legs(): Int = 2
                }
            "#,
        ),
    ]);
}

/// `Derived` inherits `Base`'s implementation, so one function implements `Shape.area` through two
/// types, and a second class makes two functions: the comparison must be made against the type
/// that is alone with its function, never against a type that shares one. The caller is in another
/// file.
#[test]
fn a_subclass_inheriting_an_implementation_reaches_it_through_the_interface() {
    expect_portable_sources(&[
        (
            "box",
            r#"
                package demo
                fun area(shape: Shape): Int = shape.area()
                fun box(): String {
                    val areas = listOf(area(Base(2)), area(Derived(3)), area(Other()), area(Base(1)))
                    return if (areas == listOf(4, 9, 7, 1)) "OK" else "FAIL"
                }
            "#,
        ),
        (
            "shapes",
            r#"
                package demo
                interface Shape { fun area(): Int }
                open class Base(private val side: Int) : Shape {
                    override fun area(): Int = side * side
                }
                class Derived(side: Int) : Base(side)
                class Other : Shape { override fun area(): Int = 7 }
            "#,
        ),
    ]);
}

/// Each of the two functions is reached from two types, so no single comparison tells them apart.
#[test]
fn two_functions_each_shared_by_two_types_read_the_interface_table() {
    expect_portable_sources(&[(
        "box",
        r#"
            package demo
            interface Tag { fun tag(): String }
            open class A : Tag { override fun tag(): String = "a" }
            class A2 : A()
            open class B : Tag { override fun tag(): String = "b" }
            class B2 : B()
            fun tags(all: List<Tag>): String {
                var got = ""
                for (tag in all) got += tag.tag()
                return got
            }
            fun box(): String {
                val got = tags(listOf(A(), B2(), A2(), B()))
                return if (got == "abab") "OK" else got
            }
        "#,
    )]);
}

/// A property reached through an interface, and a lambda converted to a functional interface,
/// each standing beside a class implementation.
#[test]
fn a_property_and_a_lambda_count_as_implementations() {
    expect_portable_sources(&[(
        "box",
        r#"
            package demo
            interface Named { val name: String }
            class Fixed : Named { override val name: String get() = "fixed" }
            class Given(override val name: String) : Named
            fun interface Step { fun next(value: Int): Int }
            class Double : Step { override fun next(value: Int): Int = value * 2 }
            fun run(step: Step, value: Int): Int = step.next(value)
            fun box(): String {
                var names = ""
                for (named in listOf<Named>(Fixed(), Given("given"))) names += named.name + ";"
                val steps = run(Double(), 5) + run(Step { it + 1 }, 5)
                return if (names == "fixed;given;" && steps == 16) "OK" else names + steps
            }
        "#,
    )]);
}

/// A value-class property implementing a generic interface property: the function at its number
/// returns the unboxed value while the member returns a reference, so the type's record must not
/// claim the member's signature for it.
#[test]
fn an_implementation_whose_signature_differs_from_the_members_is_recorded() {
    expect_portable_sources(&[(
        "box",
        r#"
            package demo
            interface Field<T> {
                val value: T
                val number: Int

                data class Unsigned(override val value: UInt, override val number: Int = 7) :
                    Field<UInt>
            }
            fun box(): String {
                val field = Field.Unsigned(3u)
                val shown = field.toString()
                return if (shown == "Unsigned(value=3, number=7)") "OK" else shown
            }
        "#,
    )]);
}
