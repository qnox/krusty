//! An override of a type-safe collection declaration (`contains`, `indexOf`, `lastIndexOf`,
//! `MutableCollection.remove`, `Map.containsKey`/`containsValue`/`get`, `MutableMap.remove`) that
//! keeps the declaration's erased descriptor is signed with its checked parameters as `Object`, as
//! kotlinc writes it. So `contains(element: T)` of a generic class carries no `Signature`, and
//! `get(key: K)` is `(Ljava/lang/Object;)TV;`. An override whose descriptor differs
//! (`contains(element: T)` with `T : CharSequence`) keeps its own generic signature, and its final
//! special bridge does the type check.
//!
//! Each comparison checks the listed members' declarations and complete bytes against kotlinc.

use super::common;

fn assert_members_like_kotlinc(source: &str, class: &str, members: &[&str]) {
    let classes = common::classes_against_kotlinc_module(&[(
        "TypeSafe.kt",
        &format!("package typesafe\n\n{source}"),
    )]);
    let class = format!("typesafe/{class}");
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

const GENERIC_LIST: &str = "class Items<T> : List<T> {
    override val size = 0
    override fun isEmpty() = true
    override fun contains(element: T) = false
    override fun containsAll(elements: Collection<T>) = false
    override fun get(index: Int): T = throw IndexOutOfBoundsException()
    override fun indexOf(element: T) = -1
    override fun lastIndexOf(element: T) = -1
    override fun iterator(): Iterator<T> = emptyList<T>().iterator()
    override fun listIterator(): ListIterator<T> = emptyList<T>().listIterator()
    override fun listIterator(index: Int): ListIterator<T> = emptyList<T>().listIterator(index)
    override fun subList(fromIndex: Int, toIndex: Int): List<T> = emptyList()
}
";

#[test]
fn generic_list_type_safe_overrides_are_signed_with_object_like_kotlinc() {
    assert_members_like_kotlinc(
        GENERIC_LIST,
        "Items",
        &[
            "public boolean contains(java.lang.Object);",
            "public int indexOf(java.lang.Object);",
            "public int lastIndexOf(java.lang.Object);",
        ],
    );
    common::expect_box_same_as_kotlinc(
        &format!(
            "{GENERIC_LIST}
fun box(): String {{
    val methods = listOf(\"contains\", \"indexOf\", \"lastIndexOf\")
        .map {{ Items::class.java.getMethod(it, Any::class.java).toGenericString() }}
    val expected = listOf(
        \"public boolean Items.contains(java.lang.Object)\",
        \"public int Items.indexOf(java.lang.Object)\",
        \"public int Items.lastIndexOf(java.lang.Object)\",
    )
    return if (methods == expected && Items<String>().indexOf(\"\") == -1) \"OK\" else methods.toString()
}}
"
        ),
        "collection_type_safe_members_generic_list",
    );
}

#[test]
fn mutable_and_nullable_type_safe_overrides_are_signed_with_object_like_kotlinc() {
    let source = "abstract class Items<T> : MutableList<T> {
    override fun remove(element: T) = false
    override fun contains(element: T) = false
}
abstract class Maybe<T> : List<T?> {
    override fun indexOf(element: T?) = -1
}
";
    assert_members_like_kotlinc(
        source,
        "Items",
        &[
            "public boolean remove(java.lang.Object);",
            "public boolean contains(java.lang.Object);",
        ],
    );
    assert_members_like_kotlinc(source, "Maybe", &["public int indexOf(java.lang.Object);"]);
}

#[test]
fn map_type_safe_overrides_are_signed_with_object_like_kotlinc() {
    let source = "abstract class Table<K, V> : Map<K, V> {
    override fun containsKey(key: K) = false
    override fun containsValue(value: V) = false
    override fun get(key: K): V? = null
}
abstract class Store<K, V> : MutableMap<K, V> {
    override fun remove(key: K): V? = null
    override fun remove(key: K, value: V): Boolean = false
}
";
    assert_members_like_kotlinc(
        source,
        "Table",
        &[
            "public boolean containsKey(java.lang.Object);",
            "public boolean containsValue(java.lang.Object);",
            "public V get(java.lang.Object);",
        ],
    );
    assert_members_like_kotlinc(
        source,
        "Store",
        &[
            "public V remove(java.lang.Object);",
            "public boolean remove(java.lang.Object, java.lang.Object);",
        ],
    );
}

#[test]
fn a_narrower_type_safe_override_keeps_its_signature_and_special_bridge_like_kotlinc() {
    let source = "abstract class Texts<T : CharSequence> : Collection<T> {
    override fun contains(element: T) = false
}
abstract class Lists<T> : List<List<T>> {
    override fun contains(element: List<T>) = false
}
abstract class Named<V> : Map<String, V> {
    override fun get(key: String): V? = null
}
";
    assert_members_like_kotlinc(
        source,
        "Texts",
        &[
            "public boolean contains(T);",
            "public final boolean contains(java.lang.Object);",
        ],
    );
    assert_members_like_kotlinc(
        source,
        "Lists",
        &[
            "public boolean contains(java.util.List<? extends T>);",
            "public final boolean contains(java.lang.Object);",
        ],
    );
    assert_members_like_kotlinc(
        source,
        "Named",
        &[
            "public V get(java.lang.String);",
            "public final V get(java.lang.Object);",
        ],
    );
}
