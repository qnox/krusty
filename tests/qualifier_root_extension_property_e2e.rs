//! The root of a qualified expression is a value when an in-scope extension property applies to an
//! implicit receiver, even when a package of the same name exists: `java.simpleName` inside
//! `fun KClass<*>.name()` reads `KClass.java`, as moshi's `KClass<*>` helpers rely on.

use super::common;

const EXTENSION_PROPERTY_ROOTS: &str = r#"import kotlin.reflect.KClass

class Box(val name: String)

val String.kotlin: Int get() = length

fun KClass<*>.simpleJavaName(): String = java.simpleName

fun String.lengthPlusOne(): Int = kotlin.plus(1)

class Holder(val type: KClass<*>) {
    fun name(): String = with(type) { java.simpleName }
}

fun box(): String {
    val direct = Box::class.simpleJavaName()
    val scoped = Holder(Box::class).name()
    val length = "abc".lengthPlusOne()
    val text = "$direct/$scoped/$length"
    return if (text == "Box/Box/4") "OK" else text
}
"#;

#[test]
fn extension_property_root_outranks_a_same_named_package() {
    common::expect_box_same_as_kotlinc(EXTENSION_PROPERTY_ROOTS, "QualifierRootExtensionProperty");
}
