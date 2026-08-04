//! JVM bridges for Kotlin collection properties.
use super::common;

fn run(src: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    common::compile_and_run_box(src, "Main", &[sl, jdk.clone()], Some(jdk.as_path()))
}

#[test]
fn collection_size_reachable_through_interface() {
    const SRC: &str = "class C : Collection<String> {\n\
        \x20   override val size: Int get() = 3\n\
        \x20   override fun isEmpty(): Boolean = false\n\
        \x20   override fun iterator(): Iterator<String> = throw UnsupportedOperationException()\n\
        \x20   override fun containsAll(elements: Collection<String>): Boolean = false\n\
        \x20   override fun contains(element: String): Boolean = false\n\
        }\n\
        fun <E> Collection<E>.forceContains(value: Any?): Boolean = contains(value as E)\n\
        fun box(): String {\n\
        \x20   val c: Collection<String> = C()\n\
        \x20   if (c.forceContains(1)) return \"wrong type\"\n\
        \x20   if (c.forceContains(null)) return \"null\"\n\
        \x20   return if (c.size == 3) \"OK\" else \"F:${c.size}\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("Collection.size bridge compiles + runs"),
        "OK"
    );
}

#[test]
fn map_keys_bridge_uses_interface_return_type() {
    const SRC: &str = "class M(private val data: Map<String, Int>) : Map<String, Int> {\n\
    override val entries: Set<Map.Entry<String, Int>> get() = data.entries\n\
    override val keys: HashSet<String> get() = HashSet(data.keys)\n\
    override val size: Int get() = data.size\n\
    override val values: Collection<Int> get() = data.values\n\
    override fun containsKey(key: String): Boolean = data.containsKey(key)\n\
    override fun containsValue(value: Int): Boolean = data.containsValue(value)\n\
    override fun get(key: String): Int? = data[key]\n\
    override fun isEmpty(): Boolean = data.isEmpty()\n\
}\n\
fun box(): String {\n\
    val value: Map<String, Int> = M(mapOf(\"a\" to 1))\n\
    return if (value.keys.single() == \"a\") \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("Map.keys bridge"), "OK");
}

#[test]
fn ordinary_generic_contains_bridge_keeps_checkcast_semantics() {
    const SRC: &str = "interface Matcher<T> { fun contains(value: T): Boolean }\n\
class StringMatcher : Matcher<String> {\n\
    override fun contains(value: String): Boolean = value == \"ok\"\n\
}\n\
fun box(): String {\n\
    val erased = StringMatcher() as Matcher<Any?>\n\
    return try {\n\
        erased.contains(1)\n\
        \"missing CCE\"\n\
    } catch (expected: ClassCastException) {\n\
        \"OK\"\n\
    }\n\
}\n";
    assert_eq!(run(SRC).expect("ordinary generic bridge"), "OK");
}

#[test]
fn collection_barrier_provenance_survives_source_interface() {
    const SRC: &str = "interface StringCollection : Collection<String> {\n\
    override fun contains(element: String): Boolean\n\
}\n\
class C : StringCollection {\n\
    override val size: Int get() = 0\n\
    override fun isEmpty(): Boolean = true\n\
    override fun iterator(): Iterator<String> = emptyList<String>().iterator()\n\
    override fun containsAll(elements: Collection<String>): Boolean = false\n\
    override fun contains(element: String): Boolean = false\n\
}\n\
fun <E> Collection<E>.forceContains(value: Any?): Boolean = contains(value as E)\n\
fun box(): String {\n\
    val c: Collection<String> = C()\n\
    if (c.forceContains(1)) return \"wrong type\"\n\
    if (c.forceContains(null)) return \"null\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("source collection interface bridge"), "OK");
}

#[test]
fn mutable_collection_remove_bridge_handles_wrong_type_and_null() {
    const SRC: &str = r#"
class NullableStrings : MutableCollection<String?> {
    override val size: Int get() = 0
    override fun isEmpty(): Boolean = true
    override fun iterator(): MutableIterator<String?> = throw UnsupportedOperationException()
    override fun contains(element: String?): Boolean = false
    override fun containsAll(elements: Collection<String?>): Boolean = false
    override fun add(element: String?): Boolean = false
    override fun addAll(elements: Collection<String?>): Boolean = false
    override fun clear() {}
    override fun remove(element: String?): Boolean = element == null
    override fun removeAll(elements: Collection<String?>): Boolean = false
    override fun retainAll(elements: Collection<String?>): Boolean = false
}

fun <E> MutableCollection<E>.forceRemove(value: Any?): Boolean = remove(value as E)

fun box(): String {
    val values: MutableCollection<String?> = NullableStrings()
    if (values.forceRemove(1)) return "wrong type"
    if (!values.forceRemove(null)) return "null did not dispatch"
    return "OK"
}
"#;
    assert_eq!(run(SRC).expect("MutableCollection.remove bridge"), "OK");
}

#[test]
fn map_key_bridges_handle_wrong_type_and_null() {
    const SRC: &str = r#"
class StringKeys(private val data: Map<String, Int>) : Map<String, Int> {
    override val entries: Set<Map.Entry<String, Int>> get() = data.entries
    override val keys: Set<String> get() = data.keys
    override val size: Int get() = data.size
    override val values: Collection<Int> get() = data.values
    override fun containsKey(key: String): Boolean = data.containsKey(key)
    override fun containsValue(value: Int): Boolean = data.containsValue(value)
    override fun get(key: String): Int? = data[key]
    override fun isEmpty(): Boolean = data.isEmpty()
}

class NullableKeys : Map<String?, Int> {
    override val entries: Set<Map.Entry<String?, Int>> get() = throw UnsupportedOperationException()
    override val keys: Set<String?> get() = throw UnsupportedOperationException()
    override val size: Int get() = 1
    override val values: Collection<Int> get() = throw UnsupportedOperationException()
    override fun containsKey(key: String?): Boolean = key == null
    override fun containsValue(value: Int): Boolean = value == 7
    override fun get(key: String?): Int? = if (key == null) 7 else null
    override fun isEmpty(): Boolean = false
}

fun <K, V> Map<K, V>.forceGet(value: Any?): V? = get(value as K)
fun <K, V> Map<K, V>.forceContainsKey(value: Any?): Boolean = containsKey(value as K)

fun box(): String {
    val strings: Map<String, Int> = StringKeys(mapOf("a" to 1))
    if (strings.forceGet(1) != null) return "wrong get type"
    if (strings.forceContainsKey(1)) return "wrong containsKey type"
    if (strings.forceGet(null) != null) return "nonnull get accepted null"
    if (strings.forceContainsKey(null)) return "nonnull containsKey accepted null"

    val nullable: Map<String?, Int> = NullableKeys()
    if (nullable.forceGet(1) != null) return "nullable wrong get type"
    if (nullable.forceContainsKey(1)) return "nullable wrong containsKey type"
    if (nullable.forceGet(null) != 7) return "nullable get did not dispatch null"
    if (!nullable.forceContainsKey(null)) return "nullable containsKey did not dispatch null"
    return "OK"
}
"#;
    assert_eq!(run(SRC).expect("Map key bridges"), "OK");
}

#[test]
fn list_barriers_keep_contains_and_index_neutral_results() {
    const SRC: &str = r#"
class Strings : List<String> {
    override val size: Int get() = 1
    override fun isEmpty(): Boolean = false
    override fun iterator(): Iterator<String> = throw UnsupportedOperationException()
    override fun contains(element: String): Boolean = element == "x"
    override fun containsAll(elements: Collection<String>): Boolean = false
    override fun get(index: Int): String = "x"
    override fun indexOf(element: String): Int = if (element == "x") 0 else -1
    override fun lastIndexOf(element: String): Int = if (element == "x") 0 else -1
    override fun listIterator(): ListIterator<String> = throw UnsupportedOperationException()
    override fun listIterator(index: Int): ListIterator<String> = throw UnsupportedOperationException()
    override fun subList(fromIndex: Int, toIndex: Int): List<String> = emptyList()
}

fun <E> List<E>.forceContains(value: Any?): Boolean = contains(value as E)
fun <E> List<E>.forceIndexOf(value: Any?): Int = indexOf(value as E)
fun <E> List<E>.forceLastIndexOf(value: Any?): Int = lastIndexOf(value as E)

fun box(): String {
    val values: List<String> = Strings()
    if (values.forceContains(1)) return "contains"
    if (values.forceIndexOf(1) != -1) return "indexOf"
    if (values.forceLastIndexOf(1) != -1) return "lastIndexOf"
    if (!values.forceContains("x")) return "valid contains"
    if (values.forceIndexOf("x") != 0) return "valid indexOf"
    if (values.forceLastIndexOf("x") != 0) return "valid lastIndexOf"
    return "OK"
}
"#;
    assert_eq!(run(SRC).expect("List collection barriers"), "OK");
}
