//! A `protected` `@JvmField` property is a protected field, as a public or internal one is a
//! public field: kotlinc emits no accessors for it, so a subclass in another file reads and writes
//! the field, and a companion's is hoisted to the outer class as a `protected static` field.

use super::common;

/// The named classes are byte-identical to kotlinc's when the module is compiled by both.
fn assert_classes_like_kotlinc(sources: &[(&str, &str)], names: &[&str]) {
    let classes = common::classes_against_kotlinc_module(sources);
    for name in names {
        let reference = classes
            .reference
            .get(*name)
            .expect("kotlinc writes the class");
        let krusty = classes.krusty.get(*name).expect("krusty writes the class");
        assert!(
            reference == krusty,
            "{}",
            classes
                .differences()
                .into_iter()
                .filter(|difference| difference.starts_with(&format!("{name} ")))
                .collect::<Vec<_>>()
                .join("\n\n")
        );
    }
}

const MEMBER_PARENT: &str = "\
sealed class Parent {
    @JvmField protected var depth: Int = 0
    @JvmField protected val slots: IntArray = IntArray(4)
}
";

const MEMBER_CHILD: &str = "\
class Child : Parent() {
    fun push(value: Int): Int {
        slots[depth] = value
        depth++
        return slots[depth - 1]
    }
}

fun box(): String = if (Child().push(7) == 7) \"OK\" else \"fail\"
";

/// The declaring class publishes protected fields and no accessors.
#[test]
fn a_protected_jvm_field_is_a_protected_field_without_accessors() {
    assert_classes_like_kotlinc(
        &[("Parent.kt", MEMBER_PARENT), ("Child.kt", MEMBER_CHILD)],
        &["Parent"],
    );
}

/// A subclass in another file reads and writes the inherited fields, so it links and runs.
#[test]
fn a_subclass_in_another_file_accesses_a_protected_jvm_field() {
    common::expect_box_ok_files_with_stdlib(
        &[("Parent.kt", MEMBER_PARENT), ("Child.kt", MEMBER_CHILD)],
        "ProtectedJvmFieldMember",
    );
}

const COMPANION_OWNER: &str = "\
open class Owner {
    companion object {
        @JvmField protected val base: Int = 1
        @JvmField protected var label: String = \"a\"
    }
}
";

const COMPANION_USER: &str = "\
class User : Owner() {
    fun describe(): String = label + base
}

fun box(): String = if (User().describe() == \"a1\") \"OK\" else \"fail\"
";

/// A companion's protected `@JvmField` is a protected static field of the outer class, which a
/// subclass in another file reads.
#[test]
fn a_protected_companion_jvm_field_is_a_protected_static_of_the_outer_class() {
    assert_classes_like_kotlinc(
        &[("Owner.kt", COMPANION_OWNER), ("User.kt", COMPANION_USER)],
        &["Owner", "Owner$Companion", "User", "UserKt"],
    );
}

/// The subclass's reads of the hoisted protected statics link and run.
#[test]
fn a_subclass_in_another_file_reads_a_protected_companion_jvm_field() {
    common::expect_box_ok_files_with_stdlib(
        &[("Owner.kt", COMPANION_OWNER), ("User.kt", COMPANION_USER)],
        "ProtectedJvmFieldCompanion",
    );
}

// The JVM grants a protected member to its package and to a subclass accessing it on its own
// receiver. The fixtures above share the default package, so they never reach that check; the
// ones below put the subclass in another package.

const PACKAGED_PARENT: &str = "\
package a

open class Parent {
    @JvmField protected var depth: Int = 0

    companion object {
        @JvmField protected var shared: Int = 7
    }
}
";

const PACKAGED_CHILD: &str = "\
package b

import a.Parent

class Child : Parent() {
    fun direct(): Int {
        depth += 1
        return depth
    }

    fun fromObject(): Int {
        object : Runnable {
            override fun run() {
                depth += 1
            }
        }.run()
        return depth
    }

    fun fromLambda(): Int {
        val add = { depth += 10 }
        add()
        return depth
    }

    fun companionRead(): Int = shared

    inner class Nested {
        fun read(): Int = depth
    }
}

fun box(): String {
    val child = Child()
    if (child.direct() != 1) return \"direct\"
    if (child.fromObject() != 2) return \"object\"
    if (child.fromLambda() != 12) return \"lambda\"
    if (child.companionRead() != 7) return \"companion\"
    if (child.Nested().read() != 12) return \"nested\"
    return \"OK\"
}
";

/// Code nested in the subclass only: the subclass's own reads name the field by its receiver's
/// class in kotlinc's output, which krusty does not yet do for any inherited field.
const BRIDGED_CHILD: &str = "\
package b

import a.Parent

class Child : Parent() {
    fun fromObject(): Runnable = object : Runnable {
        override fun run() {
            depth += 1
        }
    }

    inner class Nested {
        fun read(): Int = depth
    }
}
";

/// An anonymous object or inner class inside a subclass in another package, which the JVM does
/// not grant protected access, goes through kotlinc's `access$get<X>$p`/`access$set<X>$p` bridges
/// on the subclass.
#[test]
fn a_cross_package_subclass_bridges_its_nested_protected_jvm_field_accesses() {
    assert_classes_like_kotlinc(
        &[("a/Parent.kt", PACKAGED_PARENT), ("b/Child.kt", BRIDGED_CHILD)],
        &["a/Parent", "b/Child", "b/Child$fromObject$1", "b/Child$Nested"],
    );
}

/// A subclass in another package accesses the field directly, from a lambda, and from nested code,
/// and reads the hoisted companion field; all of it links and runs.
#[test]
fn a_cross_package_subclass_accesses_a_protected_jvm_field_from_nested_code() {
    common::expect_box_ok_files_with_stdlib(
        &[("a/Parent.kt", PACKAGED_PARENT), ("b/Child.kt", PACKAGED_CHILD)],
        "ProtectedJvmFieldCrossPackage",
    );
}
