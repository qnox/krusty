//! A collection bridge whose parameter narrows (`containsKey`, `containsValue`, `get`, …) first
//! checks the argument's type and answers `false`, `-1` or `null` for a foreign value, as kotlinc's
//! type-safe barrier does. For a value class the check tests the value class's box, which the bridge
//! then unboxes; without it a foreign key failed the bridge's `checkcast`.

use super::common;

const SRC: &str = "@JvmInline value class Key(val id: Int)\n\
    @JvmInline value class Keys(val ids: IntArray) : Map<Key, Key> {\n\
    \x20   override val size: Int get() = ids.size\n\
    \x20   override fun isEmpty(): Boolean = ids.size == 0\n\
    \x20   override fun containsKey(key: Key): Boolean = key.id < ids.size\n\
    \x20   override fun containsValue(value: Key): Boolean = value.id == 0\n\
    \x20   override fun get(key: Key): Key? = if (key.id < ids.size) key else null\n\
    \x20   override val keys: Set<Key> get() = throw UnsupportedOperationException()\n\
    \x20   override val values: Collection<Key> get() = throw UnsupportedOperationException()\n\
    \x20   override val entries: Set<Map.Entry<Key, Key>> get() = throw UnsupportedOperationException()\n\
    }\n\
    class Names : Map<String, String> {\n\
    \x20   override val size: Int get() = 0\n\
    \x20   override fun isEmpty(): Boolean = true\n\
    \x20   override fun containsKey(key: String): Boolean = false\n\
    \x20   override fun containsValue(value: String): Boolean = value == \"v\"\n\
    \x20   override fun get(key: String): String? = null\n\
    \x20   override val keys: Set<String> get() = throw UnsupportedOperationException()\n\
    \x20   override val values: Collection<String> get() = throw UnsupportedOperationException()\n\
    \x20   override val entries: Set<Map.Entry<String, String>> get() = throw UnsupportedOperationException()\n\
    }\n\
    fun box(): String {\n\
    \x20   val keys = Keys(IntArray(2)) as Map<Any, Any>\n\
    \x20   if (keys.containsKey(1) || keys.containsValue(1) || keys.get(1) != null) return \"fail foreign\"\n\
    \x20   if (!keys.containsKey(Key(1)) || !keys.containsValue(Key(0))) return \"fail key\"\n\
    \x20   val names = Names() as Map<Any, Any>\n\
    \x20   return if (names.containsValue(1)) \"fail names\" else \"OK\"\n\
    }\n";

fn assert_same_bridge(class: &str, method: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "CollectionBridge",
        SRC,
        class,
        &[common::stdlib_jar(), common::jdk_modules()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {class}.{method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference,
        "{class}.{method}"
    );
}

#[test]
fn a_value_class_map_checks_for_the_box_before_unboxing() {
    for method in [
        "boolean containsKey(java.lang.Object)",
        "boolean containsValue(java.lang.Object)",
        "java.lang.Object get(java.lang.Object)",
    ] {
        assert_same_bridge("Keys", method);
    }
}

#[test]
fn a_map_checks_the_value_type_of_contains_value() {
    assert_same_bridge("Names", "boolean containsValue(java.lang.Object)");
}

#[test]
fn collection_bridges_reject_foreign_values() {
    common::expect_box_same_as_kotlinc(SRC, "CollectionBridgeBarrier");
}

/// A value class that delegates a mutable collection forwards bulk updates (`addAll`, `putAll`)
/// to the underlying collection, and an override of `addAll` is the method a caller hits.
const BULK: &str = "@JvmInline value class IC1<T>(val list: MutableList<T>) : MutableList<T> by list\n\
    @JvmInline value class IC2<T>(val x: Int) : MutableCollection<T> {\n\
    \x20   override val size: Int get() = TODO()\n\
    \x20   override fun clear() = TODO()\n\
    \x20   override fun addAll(elements: Collection<T>) = true\n\
    \x20   override fun add(element: T) = TODO()\n\
    \x20   override fun isEmpty() = TODO()\n\
    \x20   override fun iterator() = TODO()\n\
    \x20   override fun retainAll(elements: Collection<T>) = TODO()\n\
    \x20   override fun removeAll(elements: Collection<T>) = TODO()\n\
    \x20   override fun remove(element: T) = TODO()\n\
    \x20   override fun containsAll(elements: Collection<T>) = TODO()\n\
    \x20   override fun contains(element: T) = TODO()\n\
    }\n\
    @JvmInline value class IC3<T>(val map: MutableMap<T, T>) : MutableMap<T, T> by map\n\
    fun box(): String {\n\
    \x20   val inlineList = IC1(mutableListOf(\"a1\")).also { it.addAll(0, listOf(\"a2\", \"a3\")) }\n\
    \x20   if (inlineList.list != listOf(\"a2\", \"a3\", \"a1\")) return \"Fail 1\"\n\
    \x20   if (!IC2<String>(1).addAll(setOf(\"b\"))) return \"Fail 2\"\n\
    \x20   val inlineMap = IC3(mutableMapOf(\"a\" to \"b\")).also { it.putAll(mapOf(\"b\" to \"c\", \"c\" to \"d\")) }\n\
    \x20   if (inlineMap.map != mapOf(\"a\" to \"b\", \"b\" to \"c\", \"c\" to \"d\")) return \"Fail 3\"\n\
    \x20   return \"OK\"\n\
    }\n";

#[test]
fn a_delegated_value_class_collection_forwards_bulk_updates() {
    common::expect_box_same_as_kotlinc(BULK, "BulkCollectionBridgeBarrier");
}
