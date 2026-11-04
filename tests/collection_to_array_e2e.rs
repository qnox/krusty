//! A class that implements a collection face, read-only or mutable and abstract or not, gets
//! kotlinc's public `<T> T[] toArray(T[])` and `Object[] toArray()`, which delegate to
//! `kotlin.jvm.internal.CollectionToArray`, as its last members before `<clinit>`. A class whose
//! superclass is already a collection gets neither, and a member of either shape that the class
//! declares or inherits from a non-collection supertype suppresses that one. Only the shapes count:
//! a `toArray(Array<String>)` is no generic `toArray`.
//!
//! Each comparison checks the `toArray` declarations, the members from the first generated one
//! on, and every body byte for byte against kotlinc.

use super::common;

const GENERIC: &str = "public <T> T[] toArray(T[]);";
const NON_GENERIC: &str = "public java.lang.Object[] toArray();";

const MEMBERS: &str = "    override val size = 0
    override fun isEmpty() = true
    override fun contains(element: String) = false
    override fun containsAll(elements: Collection<String>) = false
    override fun iterator(): Iterator<String> = emptyList<String>().iterator()
";

fn assert_to_array_like_kotlinc(source: &str, class: &str, expected: &[&str]) {
    let classes = common::classes_against_kotlinc_module(&[(
        "Arrays.kt",
        &format!("package arrays\n\n{source}"),
    )]);
    let class = format!("arrays/{class}");
    let (reference, krusty) = classes
        .method_declarations(&class)
        .unwrap_or_else(|| panic!("both compilers write {class}"));
    // The generated members and whatever follows them; a declared one stays in its own place.
    let tail = |declarations: &[String]| {
        let first = declarations
            .iter()
            .position(|declaration| [GENERIC, NON_GENERIC].contains(&declaration.as_str()))
            .unwrap_or(declarations.len());
        declarations[first..].to_vec()
    };
    assert_eq!(tail(&reference), tail(&krusty), "{class}");
    let declared = |declarations: &[String]| {
        declarations
            .iter()
            .filter(|declaration| declaration.contains(" toArray("))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(declared(&reference), expected, "{class}");
    assert_eq!(declared(&krusty), expected, "{class}");
    let (reference, krusty) = classes.method_blocks(&class);
    for member in expected {
        // `javap -v` spells a type parameter's implicit bound.
        let verbose = member.replace("<T>", "<T extends java.lang.Object>");
        let block = |blocks: &[(String, String)]| {
            blocks
                .iter()
                .find(|(declaration, _)| *declaration == verbose)
                .map(|(_, body)| body.clone())
                .unwrap_or_else(|| panic!("{class} declares no `{member}`"))
        };
        assert_eq!(block(&krusty), block(&reference), "{class} `{member}`");
    }
}

#[test]
fn a_read_only_collection_gets_kotlincs_to_array_pair() {
    let source = format!("class Bag : Collection<String> {{\n{MEMBERS}}}\n");
    assert_to_array_like_kotlinc(&source, "Bag", &[GENERIC, NON_GENERIC]);
    common::expect_box_same_as_kotlinc(
        &format!(
            "{source}
fun box(): String {{
    val bag = Bag()
    val typed = bag.toTypedArray()
    val copied = java.util.ArrayList(bag)
    return if (typed.size == 0 && copied.isEmpty()) \"OK\" else \"fail\"
}}
"
        ),
        "collection_to_array_read_only",
    );
}

#[test]
fn read_only_lists_and_sets_get_kotlincs_to_array_pair() {
    assert_to_array_like_kotlinc(
        "abstract class Bag : List<String>\n",
        "Bag",
        &[GENERIC, NON_GENERIC],
    );
    assert_to_array_like_kotlinc(
        "abstract class Bag<T> : Set<T>\n",
        "Bag",
        &[GENERIC, NON_GENERIC],
    );
}

#[test]
fn an_abstract_mutable_list_gets_kotlincs_to_array_pair() {
    assert_to_array_like_kotlinc(
        "abstract class Bag : MutableList<String>\n",
        "Bag",
        &[GENERIC, NON_GENERIC],
    );
}

#[test]
fn a_collection_superclass_leaves_its_subclass_without_to_array_like_kotlinc() {
    let source = format!(
        "open class Bag : Collection<String> {{\n{MEMBERS}}}\nclass Sub : Bag()\nabstract class Java : java.util.AbstractList<String>()\n"
    );
    assert_to_array_like_kotlinc(&source, "Bag", &[GENERIC, NON_GENERIC]);
    assert_to_array_like_kotlinc(&source, "Sub", &[]);
    assert_to_array_like_kotlinc(&source, "Java", &[]);
}

#[test]
fn a_matching_declared_or_inherited_to_array_suppresses_its_shape_like_kotlinc() {
    let source = "abstract class Base { open fun toArray(): Array<Any?> = arrayOfNulls(0) }
abstract class Inherits : Base(), Collection<String>
abstract class Generic : Collection<String> { fun <T> toArray(a: Array<T>): Array<T> = a }
abstract class Narrow(private val names: Array<String>) : Collection<String> { fun toArray(): Array<String> = names }
abstract class Typed : Collection<String> { fun toArray(a: Array<String>): Array<String> = a }
";
    assert_to_array_like_kotlinc(source, "Inherits", &[GENERIC]);
    assert_to_array_like_kotlinc(
        source,
        "Generic",
        &["public final <T> T[] toArray(T[]);", NON_GENERIC],
    );
    assert_to_array_like_kotlinc(
        source,
        "Narrow",
        &["public final java.lang.String[] toArray();", GENERIC],
    );
    assert_to_array_like_kotlinc(
        source,
        "Typed",
        &[
            "public final java.lang.String[] toArray(java.lang.String[]);",
            GENERIC,
            NON_GENERIC,
        ],
    );
}

#[test]
fn an_enum_collection_gets_its_to_array_pair_before_its_initializer_like_kotlinc() {
    let source = format!("enum class Bag : Collection<String> {{\n    A;\n{MEMBERS}}}\n");
    assert_to_array_like_kotlinc(&source, "Bag", &[GENERIC, NON_GENERIC]);
}

/// A concrete `MutableCollection` body: the read-only faces' throwing stubs, their bridges and
/// an abstract class's fake-override bridges stay out of the complete-class controls below.
const MUTABLE_MEMBERS: &str = "    override val size = 0
    override fun isEmpty() = true
    override fun contains(element: String) = false
    override fun containsAll(elements: Collection<String>) = false
    override fun iterator(): MutableIterator<String> = mutableListOf<String>().iterator()
    override fun add(element: String) = false
    override fun addAll(elements: Collection<String>) = false
    override fun clear() {}
    override fun remove(element: String) = false
    override fun removeAll(elements: Collection<String>) = false
    override fun retainAll(elements: Collection<String>) = false
";

/// The superclass file of the split-module control: a collection class, and a plain class with a
/// matching `toArray`. Each subclass is declared in the other file.
fn split_superclasses() -> String {
    format!(
        "package arrays

open class Bag : MutableCollection<String> {{
{MUTABLE_MEMBERS}}}

abstract class Plain {{
    open fun toArray(): Array<Any?> = arrayOfNulls(0)
}}
"
    )
}

fn split_subclasses() -> String {
    format!(
        "package arrays

class Sub : Bag()

class Inherits : Plain(), MutableCollection<String> {{
{MUTABLE_MEMBERS}}}
"
    )
}

/// A superclass in another file of the module still decides its subclass's members: its
/// collection ancestry leaves `Sub` none, and its declared `toArray()` suppresses that shape in
/// `Inherits`. The subclass file's IR does not declare either superclass.
#[test]
fn a_superclass_in_another_source_file_decides_to_array_like_kotlinc() {
    let (superclasses, subclasses) = (split_superclasses(), split_subclasses());
    let sources = [
        ("Subclasses.kt", subclasses.as_str()),
        ("Superclasses.kt", superclasses.as_str()),
    ];
    let classes = common::classes_against_kotlinc_module(&sources);
    assert_complete_classes_like_kotlinc(&classes);
    assert_declared_to_array(&classes, "arrays/Bag", &[GENERIC, NON_GENERIC]);
    assert_declared_to_array(&classes, "arrays/Sub", &[]);
    assert_declared_to_array(
        &classes,
        "arrays/Plain",
        &["public java.lang.Object[] toArray();"],
    );
    assert_declared_to_array(&classes, "arrays/Inherits", &[GENERIC]);

    let program = "package arrays

fun box(): String {
    val sub = Sub()
    val inherits = Inherits()
    val copies = java.util.ArrayList(sub).size + java.util.ArrayList(inherits).size
    val typed = sub.toTypedArray().size + inherits.toTypedArray().size
    val declared = listOf(sub.javaClass, inherits.javaClass).map { type ->
        type.declaredMethods.filter { it.name == \"toArray\" }.map { it.toGenericString() }.sorted()
    }
    return \"$copies $typed $declared\"
}
";
    let runtime = [
        ("Subclasses.kt", subclasses.as_str()),
        ("Superclasses.kt", superclasses.as_str()),
        ("Box.kt", program),
    ];
    let reference = common::kotlinc_box_files_result(&runtime, "arrays.BoxKt");
    assert_eq!(
        reference, "0 0 [[], [public <T> T[] arrays.Inherits.toArray(T[])]]",
        "kotlinc fixture"
    );
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&runtime).expect("krusty compiles and runs"),
        reference
    );
}

/// Both compilers write the same classes, and each one's complete `javap -v -p` listing (header,
/// every member with its code, debug tables and attributes, and the class attributes) is the
/// same. The one deferred difference is the generic signature of a declared override of a
/// `Collection`-parameter member: kotlinc signs it by the overridden declaration's
/// `Collection<?>`/`Collection<? extends E>`, which a later slice realizes, so kotlinc's spelling
/// of exactly those parameters is read as krusty's.
fn assert_complete_classes_like_kotlinc(classes: &common::ClassSets) {
    let names =
        |set: &std::collections::BTreeMap<String, Vec<u8>>| set.keys().cloned().collect::<Vec<_>>();
    assert_eq!(names(&classes.krusty), names(&classes.reference));
    let deferred = [
        (
            "java.util.Collection<?>",
            "java.util.Collection<java.lang.String>",
        ),
        (
            "java.util.Collection<? extends java.lang.String>",
            "java.util.Collection<java.lang.String>",
        ),
        (
            "Ljava/util/Collection<*>;",
            "Ljava/util/Collection<Ljava/lang/String;>;",
        ),
        (
            "Ljava/util/Collection<+Ljava/lang/String;>;",
            "Ljava/util/Collection<Ljava/lang/String;>;",
        ),
    ];
    for class in classes.reference.keys() {
        let (mut reference, krusty) = classes.class_listing(class);
        for (kotlinc, realized) in deferred {
            reference = reference.replace(kotlinc, realized);
        }
        assert_eq!(krusty, reference, "{class}");
    }
}

/// The `toArray` declarations `class` has in each compiler's output, in class-file order.
fn assert_declared_to_array(classes: &common::ClassSets, class: &str, expected: &[&str]) {
    let (reference, krusty) = classes
        .method_declarations(class)
        .unwrap_or_else(|| panic!("both compilers write {class}"));
    let declared = |declarations: &[String]| {
        declarations
            .iter()
            .filter(|declaration| declaration.contains(" toArray("))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(declared(&reference), expected, "{class}");
    assert_eq!(declared(&krusty), expected, "{class}");
}

/// A Java superclass whose `toArray()` is package-private.
const JAVA_BASE: &str = "package lib;

public abstract class JBase {
    Object[] toArray() { return new Object[0]; }
}
";

/// A Java class of another package between a Kotlin class of `lib` and `JBase`.
const JAVA_MIDDLE: &str = "package other;

public abstract class JMiddle extends lib.JBase {}
";

/// Both compilers compile `package` with a concrete `Bag : superclass(), MutableCollection<String>`
/// against the javac-built `JBase` and `JMiddle`: every class they write is byte-identical, and
/// `Bag` declares exactly `expected`.
fn assert_java_superclass_like_kotlinc(package: &str, superclass: &str, expected: &[&str]) {
    let (java, _) = common::javac_compile(
        &[
            ("JBase.java".to_string(), JAVA_BASE.to_string()),
            ("JMiddle.java".to_string(), JAVA_MIDDLE.to_string()),
        ],
        &[],
    )
    .expect("javac compiles the Java superclasses");
    let source = format!(
        "package {package}\n\nclass Bag : {superclass}(), MutableCollection<String> {{\n{MUTABLE_MEMBERS}}}\n"
    );
    let classes = common::classes_against_kotlinc_module_cp(&[("Bag.kt", &source)], &[java]);
    assert_complete_classes_like_kotlinc(&classes);
    assert_declared_to_array(&classes, &format!("{package}/Bag"), expected);
}

/// In its own package the package-private `toArray()` is inherited and suppresses that shape.
#[test]
fn a_package_private_java_to_array_in_the_same_package_suppresses_its_shape_like_kotlinc() {
    assert_java_superclass_like_kotlinc("lib", "JBase", &[GENERIC]);
}

/// Into another package it is not inherited and suppresses nothing.
#[test]
fn a_package_private_java_to_array_in_another_package_suppresses_nothing_like_kotlinc() {
    assert_java_superclass_like_kotlinc("app", "lib.JBase", &[GENERIC, NON_GENERIC]);
}

/// A superclass path that leaves the package does not carry it back in: `other.JMiddle` does not
/// inherit it, so neither does a class of `lib` that extends `JMiddle`.
#[test]
fn a_package_private_java_to_array_across_another_package_suppresses_nothing_like_kotlinc() {
    assert_java_superclass_like_kotlinc("lib", "other.JMiddle", &[GENERIC, NON_GENERIC]);
}
