//! A functional-interface constructor written with explicit type arguments is applied to exactly
//! those arguments, whatever the lambda would let inference conclude, also where the declaration's
//! type is inferred from the initializer: `val`, expression-bodied functions, and delegates.

use super::common;

#[test]
fn explicit_arguments_fix_the_type_of_an_inferred_property() {
    common::expect_box_ok_with_stdlib(
        r#"
fun interface Reader<in A, out B> { fun read(a: A): B }

class Level(val name: String)

val reader = Reader<Any?, Level> { _ -> Level("OK") }
val name = reader.read(null).name

fun box(): String = name
"#,
        "sam_constructor_explicit_arguments_property",
    );
}

#[test]
fn explicit_arguments_fix_the_type_of_an_expression_bodied_function() {
    common::expect_box_ok_with_stdlib(
        r#"
fun interface Reader<A, B> { fun read(a: A): B }

class Level(val name: String)

fun reader() = Reader<Any?, Level> { _ -> Level("OK") }

fun box(): String = reader().read(null).name
"#,
        "sam_constructor_explicit_arguments_function",
    );
}

#[test]
fn a_delegate_built_by_an_inferred_functional_constructor_types_its_property() {
    common::expect_box_ok_with_stdlib(
        r#"
import kotlin.properties.ReadOnlyProperty

class Level(val name: String) {
    fun mergeWith(other: Level) = Level(name + other.name)
}

internal fun level(name: String, config: StringBuilder.() -> Unit) =
    ReadOnlyProperty<Any?, Level> { _, _ ->
        val builder = StringBuilder(name)
        config(builder)
        Level(builder.toString())
    }

val first by level("O") {}
val second by level("K") {}
val merged = first.mergeWith(second)

fun box(): String = merged.name
"#,
        "sam_constructor_explicit_arguments_delegate",
    );
}
