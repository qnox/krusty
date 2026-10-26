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
