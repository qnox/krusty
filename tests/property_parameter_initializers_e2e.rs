//! Kotlin 2 resolves a `val`/`var` primary-constructor parameter named in a property initializer or
//! an `init` block to the property it declares, so the constructor reads the field (or calls the
//! accessor of an open property). Plain parameters, supertype arguments, and default values keep
//! reading the parameter.

use super::common;

#[test]
fn initializers_read_property_parameters_through_the_property_like_kotlinc() {
    let src = r#"
class Value

open class Base(val base: Value)

class Plain(val x: Value, y: Value) : Base(x) {
    var picked = y
    val z: Value
    init {
        z = x
        picked = x
    }
}

class Hidden(private val x: Value) { val y = x }

open class Open(open val x: Value) { val y = x }

class Defaulted(val x: Value, val z: Value = x)

class Mutated(var x: Value, replacement: Value) {
    init { x = replacement }
    val y = x
}

class Shadowed(val x: Value, local: Value) {
    init {
        val x = local
        sink(x)
    }
    val z = x
}

class SameName(x: Value) { val x = x }

fun sink(value: Value) {}
"#;
    common::assert_classes_identical_to_kotlinc(
        "PropertyParameterInitializers",
        src,
        &[
            "Value",
            "Base",
            "Plain",
            "Hidden",
            "Open",
            "Defaulted",
            "Mutated",
            "Shadowed",
            "SameName",
            "PropertyParameterInitializersKt",
        ],
    );
}
