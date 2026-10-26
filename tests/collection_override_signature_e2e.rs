//! An override of a collection declaration that kotlinc realizes with a signature policy takes
//! that policy's generic signature: `containsAll`, `removeAll` and `retainAll` star-project their
//! collection parameter (`Collection<?>`), and `addAll` (both of `MutableList`'s) and `putAll`
//! write every declaration-site variance (`Collection<? extends E>`, `Map<? extends K, ? extends
//! V>`). The policy comes from the overridden declaration, so it applies through a mutable face, a
//! read-only one, a generic class and a Java collection superclass alike. A value class's static
//! replacement of such an override (`addAll-impl`) overrides nothing: it keeps its ordinary
//! signature, the moved receiver first (`(ILjava/util/Collection<+TT;>;)Z`).
//!
//! Each comparison checks the listed members' declarations and complete bytes against kotlinc.

use super::common;

fn assert_members_like_kotlinc(source: &str, class: &str, members: &[&str]) {
    let classes = common::classes_against_kotlinc_module(&[(
        "Overrides.kt",
        &format!("package overrides\n\n{source}"),
    )]);
    let class = format!("overrides/{class}");
    let (reference, krusty) = classes.method_blocks(&class);
    for member in members {
        let block = |blocks: &[(String, String)]| {
            blocks
                .iter()
                .find(|(declaration, _)| declaration == member)
                .map(|(_, body)| body.clone())
                .unwrap_or_else(|| panic!("{class} declares no `{member}`"))
        };
        assert_eq!(block(&krusty), block(&reference), "{class} `{member}`");
    }
}

const READ_ONLY: &str = "class Bag : Collection<String> {
    override val size = 0
    override fun isEmpty() = true
    override fun contains(element: String) = false
    override fun containsAll(elements: Collection<String>) = elements.isEmpty()
    override fun iterator(): Iterator<String> = emptyList<String>().iterator()
}
";

const MUTABLE: &str = "abstract class Bag : MutableCollection<String> {
    override fun containsAll(elements: Collection<String>) = false
    override fun addAll(elements: Collection<String>) = false
    override fun removeAll(elements: Collection<String>) = false
    override fun retainAll(elements: Collection<String>) = false
}
";

#[test]
fn a_read_only_contains_all_override_is_star_projected_like_kotlinc() {
    assert_members_like_kotlinc(
        READ_ONLY,
        "Bag",
        &["public boolean containsAll(java.util.Collection<?>);"],
    );
    common::expect_box_same_as_kotlinc(
        &format!(
            "{READ_ONLY}
fun box(): String {{
    val method = Bag::class.java.getMethod(\"containsAll\", Collection::class.java)
    val signature = method.toGenericString()
    val expected = \"public boolean Bag.containsAll(java.util.Collection<?>)\"
    return if (signature == expected && Bag().containsAll(emptyList())) \"OK\" else signature
}}
"
        ),
        "collection_override_signature_read_only",
    );
}

#[test]
fn mutable_collection_overrides_take_their_declarations_policies_like_kotlinc() {
    assert_members_like_kotlinc(
        MUTABLE,
        "Bag",
        &[
            "public boolean containsAll(java.util.Collection<?>);",
            "public boolean addAll(java.util.Collection<? extends java.lang.String>);",
            "public boolean removeAll(java.util.Collection<?>);",
            "public boolean retainAll(java.util.Collection<?>);",
        ],
    );
}

#[test]
fn list_set_and_generic_overrides_take_their_declarations_policies_like_kotlinc() {
    let source = "abstract class Ints : MutableList<Int> {
    override fun addAll(elements: Collection<Int>) = false
    override fun addAll(index: Int, elements: Collection<Int>) = false
    override fun removeAll(elements: Collection<Int>) = false
    override fun containsAll(elements: Collection<Int>) = false
}
abstract class Texts : Set<CharSequence> {
    override fun containsAll(elements: Collection<CharSequence>) = false
}
abstract class Items<T> : MutableCollection<T> {
    override fun addAll(elements: Collection<T>) = false
    override fun retainAll(elements: Collection<T>) = false
}
";
    assert_members_like_kotlinc(
        source,
        "Ints",
        &[
            "public boolean addAll(java.util.Collection<? extends java.lang.Integer>);",
            "public boolean addAll(int, java.util.Collection<? extends java.lang.Integer>);",
            "public boolean removeAll(java.util.Collection<?>);",
            "public boolean containsAll(java.util.Collection<?>);",
        ],
    );
    assert_members_like_kotlinc(
        source,
        "Texts",
        &["public boolean containsAll(java.util.Collection<?>);"],
    );
    assert_members_like_kotlinc(
        source,
        "Items",
        &[
            "public boolean addAll(java.util.Collection<? extends T>);",
            "public boolean retainAll(java.util.Collection<?>);",
        ],
    );
}

#[test]
fn map_and_java_superclass_overrides_take_their_declarations_policies_like_kotlinc() {
    let source = "abstract class Table : MutableMap<String, Int> {
    override fun putAll(from: Map<out String, Int>) {}
}
class Names : java.util.ArrayList<String>() {
    override fun removeAll(elements: Collection<String>) = false
    override fun addAll(elements: Collection<String>) = false
}
";
    assert_members_like_kotlinc(
        source,
        "Table",
        &["public void putAll(java.util.Map<? extends java.lang.String, ? extends java.lang.Integer>);"],
    );
    assert_members_like_kotlinc(
        source,
        "Names",
        &[
            "public boolean removeAll(java.util.Collection<?>);",
            "public boolean addAll(java.util.Collection<? extends java.lang.String>);",
        ],
    );
}

const VALUE_CLASSES: &str = "@JvmInline value class Wrapped<T>(val x: Int) : MutableCollection<T> {
    override val size: Int get() = 0
    override fun clear() {}
    override fun addAll(elements: Collection<T>) = true
    override fun add(element: T) = false
    override fun isEmpty() = true
    override fun iterator(): MutableIterator<T> = mutableListOf<T>().iterator()
    override fun retainAll(elements: Collection<T>) = false
    override fun removeAll(elements: Collection<T>) = false
    override fun remove(element: T) = false
    override fun containsAll(elements: Collection<T>) = elements.isEmpty()
    override fun contains(element: T) = false
}

@JvmInline value class Names(val x: Int) : MutableCollection<String> {
    override val size: Int get() = 0
    override fun clear() {}
    override fun addAll(elements: Collection<String>) = true
    override fun add(element: String) = false
    override fun isEmpty() = true
    override fun iterator(): MutableIterator<String> = mutableListOf<String>().iterator()
    override fun retainAll(elements: Collection<String>) = false
    override fun removeAll(elements: Collection<String>) = false
    override fun remove(element: String) = false
    override fun containsAll(elements: Collection<String>) = elements.isEmpty()
    override fun contains(element: String) = false
}
";

#[test]
fn a_value_class_static_replacement_keeps_its_ordinary_signature_like_kotlinc() {
    assert_members_like_kotlinc(
        VALUE_CLASSES,
        "Wrapped",
        &[
            "public static boolean addAll-impl(int, java.util.Collection<? extends T>);",
            "public static boolean retainAll-impl(int, java.util.Collection<? extends T>);",
            "public static boolean removeAll-impl(int, java.util.Collection<? extends T>);",
            "public static boolean containsAll-impl(int, java.util.Collection<? extends T>);",
        ],
    );
    assert_members_like_kotlinc(
        VALUE_CLASSES,
        "Names",
        &[
            "public static boolean addAll-impl(int, java.util.Collection<java.lang.String>);",
            "public static boolean retainAll-impl(int, java.util.Collection<java.lang.String>);",
            "public static boolean removeAll-impl(int, java.util.Collection<java.lang.String>);",
            "public static boolean containsAll-impl(int, java.util.Collection<java.lang.String>);",
        ],
    );
    common::expect_box_same_as_kotlinc(
        &format!(
            "{VALUE_CLASSES}
fun box(): String {{
    val method = Wrapped::class.java.getMethod(\"addAll-impl\", Int::class.javaPrimitiveType, Collection::class.java)
    val parameters = method.genericParameterTypes.joinToString()
    if (parameters != \"int, java.util.Collection<? extends T>\") return parameters
    return if (Wrapped<String>(1).addAll(setOf(\"b\"))) \"OK\" else \"addAll\"
}}
"
        ),
        "collection_override_signature_value_class",
    );
}
