//! Private members of a companion object, used from the class that contains it.
//!
//! Kotlin's private visibility is lexical: a declaration that is private inside a companion object
//! is visible everywhere in the companion's containing class, so a private class nested in a private
//! companion can be constructed and read from the outer class's own methods. A private or protected
//! companion also keeps that visibility on the outer class's `Companion` field, and another class
//! that may not read that field goes through a synthetic accessor.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty, and each
//! class file is compared byte for byte.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()]) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

/// The reported shape: a private data class inside a private companion, built and updated from a
/// method of the containing class.
const PRIVATE_NESTED_IN_PRIVATE_COMPANION: &str = r#"
class Raster {
    fun scan(): Float {
        val e = Edge(1f, 0.5f, 3, -1)
        e.x += e.invSlope
        return e.x + e.zMax + e.winding
    }
    private companion object {
        private data class Edge(var x: Float, val invSlope: Float, val zMax: Int, val winding: Int)
    }
}
"#;

#[test]
fn a_private_class_of_a_private_companion_is_usable_from_the_containing_class() {
    for class in ["Raster", "Raster$Companion", "Raster$Companion$Edge"] {
        byte_identical(
            "private_nested_in_private_companion",
            PRIVATE_NESTED_IN_PRIVATE_COMPANION,
            class,
        );
    }
}

const COMPANION_VISIBILITIES: &str = r#"
class PrivateHolder {
    fun read() = twice(2)
    private companion object {
        private fun twice(value: Int) = value * 2
    }
}
open class ProtectedHolder {
    protected companion object
}
"#;

#[test]
fn the_companion_field_keeps_the_companion_visibility() {
    for class in ["PrivateHolder", "ProtectedHolder"] {
        byte_identical("companion_visibilities", COMPANION_VISIBILITIES, class);
    }
}

/// Every other JVM class that reads a private companion (a nested or inner class, an object
/// expression, the companion itself) goes through the owner's `access$getCompanion$p`.
const PRIVATE_COMPANION_FROM_OTHER_CLASSES: &str = r#"
class Outer {
    private companion object {
        val result = "OK"
        fun self() = Outer.Companion
        fun viaName() = Outer.result
    }
    class Nested { fun foo() = result }
    inner class Inner { fun foo() = result + Companion.result }
    fun obj() = object { fun f() = result }.f()
    fun lam() = { result }
    fun direct() = result
}
"#;

#[test]
fn other_classes_read_a_private_companion_through_its_accessor() {
    for class in [
        "Outer",
        "Outer$Companion",
        "Outer$Nested",
        "Outer$Inner",
        "Outer$obj$1",
    ] {
        byte_identical(
            "private_companion_from_other_classes",
            PRIVATE_COMPANION_FROM_OTHER_CLASSES,
            class,
        );
    }
}

/// A protected companion declared in another package. A subclass reads its field directly; a
/// nested class, an object expression and an unrelated class's companion initializer cannot, and
/// read it through `access$getCompanion$p$s<hash>` on the class that grants the access: the
/// enclosing subclass, else the enclosing class's companion that subclasses the holder.
const PROTECTED_BASE: &str = r#"
package a

open class A {
    protected companion object {
        fun getO() = "O"
    }
}
"#;

const PROTECTED_USES: &str = r#"
package b

import a.A

class Outer : A() {
    private companion object {
        fun getK() = "K"
    }
    val direct = getO()
    class Nested {
        val test = getO() + getK()
        fun foo() = object {
            override fun toString() = getO() + getK()
        }
    }
}

class Unrelated {
    companion object : A() {
        val ok = getO()
    }
}
"#;

#[test]
fn other_classes_read_an_inherited_protected_companion_through_the_granting_class() {
    let classes = common::classes_against_kotlinc_module(&[
        ("a/A.kt", PROTECTED_BASE),
        ("b/Uses.kt", PROTECTED_USES),
    ]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for (class, reference) in &classes.reference {
        assert!(
            classes.krusty[class] == *reference,
            "{class}: krusty's bytes differ from kotlinc's"
        );
    }
}

const PROTECTED_USE_CLASSES: &[&str] = &[
    "b/Outer",
    "b/Outer$Companion",
    "b/Outer$Nested",
    "b/Outer$Nested$foo$1",
    "b/Unrelated",
    "b/Unrelated$Companion",
];

/// The same reads against a holder compiled beforehand by kotlinc: its classifier record, read from
/// Kotlin metadata, publishes the same companion edge and visibility a source holder does.
#[test]
fn a_protected_companion_of_a_kotlinc_built_dependency_is_read_like_a_source_one() {
    let library = common::kotlinc_lib_out(&[("A.kt", PROTECTED_BASE)])
        .expect("reference kotlinc is provisioned");
    common::assert_classes_identical_to_kotlinc_against(
        "Uses",
        PROTECTED_USES,
        PROTECTED_USE_CLASSES,
        &[library],
    );
}

/// The same reads against a holder krusty compiled beforehand.
#[test]
fn a_protected_companion_of_a_krusty_built_dependency_is_read_like_a_source_one() {
    let library = common::compile_lib("protected_companion_base", PROTECTED_BASE)
        .expect("krusty compiles the holder");
    common::assert_classes_identical_to_kotlinc_against(
        "Uses",
        PROTECTED_USES,
        PROTECTED_USE_CLASSES,
        &[library],
    );
}

/// A private companion's `const val` and `@JvmField` property become fields of the outer class,
/// and kotlinc deprecates each one that is not private for Java, whatever its visibility:
/// `@java.lang.Deprecated` and the `Deprecated` attribute. A private `const val` stays a private
/// field and an internal or protected companion's fields are not deprecated.
#[test]
fn a_private_companions_non_private_fields_are_deprecated_on_the_outer_class() {
    common::assert_classes_identical_to_kotlinc(
        "PrivateCompanionFields",
        r#"
class Holder {
    fun all() = LIMIT + label.length + hidden + NAME.length
    private companion object {
        const val LIMIT = 3600L
        @JvmField val label = "label"
        private const val hidden = 3
        internal const val NAME = "name"
    }
}

class Leaked {
    fun all() = tag.length + LIMIT
    private companion object {
        @JvmField internal val tag = "t"
        internal const val LIMIT = 2
    }
}

class Shared {
    fun all() = LIMIT
    internal companion object {
        const val LIMIT = 1
    }
}

open class Guarded {
    fun all() = LIMIT
    protected companion object {
        const val LIMIT = 1
    }
}
"#,
        &["Holder", "Leaked", "Shared", "Guarded"],
    );
}

/// An interface's members are public, so kotlinc makes its private companion public in bytecode
/// (`specialCaseVisibility`): its `InnerClasses` rows are public, and the interface's `Companion`
/// field is public and synthetic, so it carries no nullability annotation. Each constant the
/// companion moves onto the interface is deprecated as on a class. A public companion's field is
/// an ordinary one, on an interface and on an annotation class alike.
#[test]
fn an_interfaces_private_companion_is_public_and_synthetic_in_bytecode() {
    common::assert_classes_identical_to_kotlinc(
        "InterfacePrivateCompanion",
        r#"
interface Keyed {
    fun all() = LIMIT + label.length + named()
    private companion object {
        const val LIMIT = 3
        val label = "label"
        fun named() = 1
    }
}

interface Open {
    fun all() = LIMIT
    companion object {
        const val LIMIT = 1
    }
}

annotation class Tagged {
    companion object {
        const val LIMIT = 1
        const val NAME = "tagged"
    }
}
"#,
        &[
            "Keyed",
            "Keyed$Companion",
            "Open",
            "Open$Companion",
            "Tagged",
            "Tagged$Companion",
        ],
    );
}

/// The annotation interface and its companion load and verify on the JVM: reading the companion
/// goes through the annotation's `Companion` field, and the class literal is an annotation type.
#[test]
fn an_annotation_class_companion_loads_through_its_field() {
    common::expect_box_ok_with_stdlib(
        r#"
annotation class Tagged {
    companion object {
        const val LIMIT = 1
        fun named() = "tagged"
    }
}

fun box(): String {
    val companion = Tagged
    if (!Tagged::class.java.isAnnotation) return "fail: not an annotation"
    if (companion.named() != "tagged") return "fail: companion"
    return if (Tagged.LIMIT == 1) "OK" else "fail: limit"
}
"#,
        "AnnotationCompanionLoads",
    );
}
