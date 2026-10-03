//! A generic local extension participates in a callable reference after its receiver is
//! specialized through the symbol hierarchy and its formal bounds hold. Source order still
//! decides visibility: a reference written before the local function binds the same-named
//! extension property. The classifiers here are repository-owned and invariant.

use super::common::expect_box_same_as_kotlinc;

#[test]
fn a_later_generic_local_extension_wins_an_unbound_reference() {
    expect_box_same_as_kotlinc(
        r#"
class Items<T>

var result = "failed"

val <T> Items<T>.foo: T
    get() = "not" as T

val <T> Items<T>.bar: T.() -> String
    get() = { "fai" }

val <T> (Items<T>.() -> T).baz: T
    get() = this(Items())

fun box(): String {
    fun <T> test(): Items<T>.() -> T = Items<T>::foo
    result = test<String>()(Items())

    fun <T> test2(): Items<T>.() -> (T.() -> String) = Items<T>::bar
    result += test2<String>()(Items())("")

    fun <T> Items<T>.foo(): T { return "led" as T }
    fun <T> test3(): () -> T = Items<T>::foo::baz
    result += test3<String>()()

    return if (result == "notfailed") "OK" else result
}
"#,
        "GenericLocalExtensionRef",
    );
}

#[test]
fn a_generic_local_extension_wins_a_bound_reference() {
    expect_box_same_as_kotlinc(
        r#"
class Items<T>(val value: T)

val <T> Items<T>.item: String
    get() = "prop"

fun box(): String {
    fun <T> Items<T>.item(): String = "ext"
    val xs = Items("a")
    val read: () -> String = xs::item
    return if (read() == "ext") "OK" else read()
}
"#,
        "BoundGenericLocalExtensionRef",
    );
}

#[test]
fn an_earlier_reference_still_binds_the_extension_property() {
    expect_box_same_as_kotlinc(
        r#"
class Items<T>(val value: T)

val <T> Items<T>.item: String
    get() = "prop"

fun box(): String {
    val xs = Items("a")
    val read: () -> String = xs::item
    fun <T> Items<T>.item(): String = "ext"
    return if (read() == "prop") "OK" else read()
}
"#,
        "EarlierExtensionPropertyRef",
    );
}

#[test]
fn a_derived_receiver_specializes_a_base_local_extension() {
    expect_box_same_as_kotlinc(
        r#"
open class Base<T>
class Derived<U> : Base<U>()

fun box(): String {
    fun <T> Base<T>.pick(): String = "base"
    val value = Derived<String>()
    val bound: () -> String = value::pick
    fun <T> unbound(): Derived<T>.() -> String = Derived<T>::pick
    val through = unbound<String>()
    return if (bound() == "base" && through(value) == "base") "OK" else bound() + through(value)
}
"#,
        "DerivedBaseLocalExtensionRef",
    );
}

#[test]
fn a_bound_violation_keeps_the_extension_property() {
    expect_box_same_as_kotlinc(
        r#"
interface Marker
class Box<T>(val value: T)
class Plain

val <T> Box<T>.pick: String
    get() = "prop"

fun box(): String {
    fun <T : Marker> Box<T>.pick(): String = "ext"
    val box = Box(Plain())
    val read: () -> String = box::pick
    return if (read() == "prop") "OK" else read()
}
"#,
        "BoundedLocalExtensionPropertyFallback",
    );
}
