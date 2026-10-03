//! A generic local extension participates in a callable reference after its receiver is
//! specialized. Source order still decides visibility: a reference written before the local
//! function binds the same-named extension property.

use super::common::expect_box_same_as_kotlinc;

#[test]
fn a_later_generic_local_extension_wins_an_unbound_reference() {
    expect_box_same_as_kotlinc(
        r#"
var result = "failed"

val <T> List<T>.foo: T
    get() = "not" as T

val <T> List<T>.bar: T.() -> String
    get() = { "fai" }

val <T> (List<T>.() -> T).baz: T
    get() = this(listOf())

fun box(): String {
    fun <T> test(): List<T>.() -> T = List<T>::foo
    result = test<String>()(listOf())

    fun <T> test2(): List<T>.() -> (T.() -> String) = List<T>::bar
    result += test2<String>()(listOf())("")

    fun <T> List<T>.foo(): T { return "led" as T }
    fun <T> test3(): () -> T = List<T>::foo::baz
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
val <T> List<T>.item: String
    get() = "prop"

fun box(): String {
    fun <T> List<T>.item(): String = "ext"
    val xs = listOf("a")
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
val <T> List<T>.item: String
    get() = "prop"

fun box(): String {
    val xs = listOf("a")
    val read: () -> String = xs::item
    fun <T> List<T>.item(): String = "ext"
    return if (read() == "prop") "OK" else read()
}
"#,
        "EarlierExtensionPropertyRef",
    );
}
