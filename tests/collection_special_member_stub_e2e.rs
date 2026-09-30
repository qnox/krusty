//! JVM bridges for Kotlin collection properties.
use super::common;

fn run(src: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    common::compile_and_run_box(src, "Main", &[sl, jdk.clone()], Some(jdk.as_path()))
}

fn assert_byte_equal(tag: &str, source: &str, class: &str) {
    let Some(result) =
        common::byte_diff_against_kotlinc_cp(tag, source, class, &[common::stdlib_jar()])
    else {
        eprintln!("skip: kotlinc unavailable for {tag}");
        return;
    };
    assert_eq!(result, Ok(()), "{tag} must be byte-identical to kotlinc");
}

#[test]
fn mapped_builtin_property_and_function_calls_match_kotlinc_bytes() {
    assert_byte_equal(
        "MappedBuiltinCallBytes",
        "fun collectionSize(value: Collection<String>): Int = value.size\n",
        "MappedBuiltinCallBytesKt",
    );
    assert_byte_equal(
        "MappedMapKeysCallBytes",
        "fun mapKeys(value: Map<String, Int>): Set<String> = value.keys\n",
        "MappedMapKeysCallBytesKt",
    );
    assert_byte_equal(
        "MappedMapEntriesCallBytes",
        "fun mapEntries(value: Map<String, Int>): Set<*> = value.entries\n",
        "MappedMapEntriesCallBytesKt",
    );
    assert_byte_equal(
        "MappedRemoveAtCallBytes",
        "fun removeAt(value: MutableList<Any?>): Any? = value.removeAt(0)\n",
        "MappedRemoveAtCallBytesKt",
    );
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

/// `containsValue(Int)` and `indexOf(Int)` erase to `Object`. Null and a foreign wrapper return
/// the neutral result; an `Integer` is unboxed and delegated.
#[test]
fn primitive_collection_bridges_reject_null_and_foreign_wrappers() {
    const SRC: &str = r#"
private object IntValues : Map<String, Int> {
    override val size: Int get() = 1
    override val entries: Set<Map.Entry<String, Int>> get() = emptySet()
    override val keys: Set<String> get() = emptySet()
    override val values: Collection<Int> get() = emptyList()
    override fun containsKey(key: String): Boolean = false
    override fun containsValue(value: Int): Boolean = true
    override fun get(key: String): Int? = null
    override fun isEmpty(): Boolean = false
}

private object Ints : List<Int> {
    override val size: Int get() = 1
    override fun isEmpty(): Boolean = false
    override fun iterator(): Iterator<Int> = throw UnsupportedOperationException()
    override fun contains(element: Int): Boolean = true
    override fun containsAll(elements: Collection<Int>): Boolean = false
    override fun get(index: Int): Int = 3
    override fun indexOf(element: Int): Int = if (element == 3) 0 else -2
    override fun lastIndexOf(element: Int): Int = if (element == 3) 0 else -2
    override fun listIterator(): ListIterator<Int> = throw UnsupportedOperationException()
    override fun listIterator(index: Int): ListIterator<Int> = throw UnsupportedOperationException()
    override fun subList(fromIndex: Int, toIndex: Int): List<Int> = emptyList()
}

private object IntBag : MutableCollection<Int> {
    override val size: Int get() = 0
    override fun isEmpty(): Boolean = true
    override fun iterator(): MutableIterator<Int> = throw UnsupportedOperationException()
    override fun contains(element: Int): Boolean = false
    override fun containsAll(elements: Collection<Int>): Boolean = false
    override fun add(element: Int): Boolean = element == 3
    override fun addAll(elements: Collection<Int>): Boolean = false
    override fun clear() {}
    override fun remove(element: Int): Boolean = false
    override fun removeAll(elements: Collection<Int>): Boolean = false
    override fun retainAll(elements: Collection<Int>): Boolean = false
}

fun box(): String {
    val values = IntValues as Map<Any?, Any?>
    if (values.containsValue(null)) return "value null"
    if (values.containsValue("x")) return "value type"
    if (!values.containsValue(3)) return "value int"
    if (!values.containsValue(4)) return "value other int"

    val ints = Ints as List<Any?>
    if (ints.contains(null)) return "contains null"
    if (ints.contains("x")) return "contains type"
    if (!ints.contains(3)) return "contains int"
    if (ints.indexOf(null) != -1) return "index null"
    if (ints.indexOf("x") != -1) return "index type"
    if (ints.indexOf(4) != -2) return "index other int"
    if (ints.indexOf(3) != 0) return "index int"
    if (ints.lastIndexOf(null) != -1) return "last null"
    if (ints.lastIndexOf(3) != 0) return "last int"

    val bag = IntBag as MutableCollection<Any?>
    try {
        bag.add(null)
        return "add null did not fail"
    } catch (_: NullPointerException) {}
    try {
        bag.add("x")
        return "add foreign wrapper did not fail"
    } catch (_: ClassCastException) {}
    if (!bag.add(3)) return "add int"
    return "OK"
}
"#;
    assert_eq!(run(SRC).expect("primitive collection bridges"), "OK");
}

/// `E : Map.Entry` still erases `contains` to `Object`. A foreign instance returns false; the
/// bridge does not checkcast to `Map.Entry` and throw.
#[test]
fn abstract_set_contains_rejects_a_foreign_entry() {
    const SRC: &str = r#"
class MySet<K, V, E : Map.Entry<K, V>> : AbstractSet<E>() {
    override fun contains(element: E): Boolean = element.key !== null
    override val size: Int get() = 0
    override fun isEmpty(): Boolean = false
    override fun containsAll(elements: Collection<E>): Boolean = false
    override fun iterator(): Iterator<E> = emptyList<E>().iterator()
}

fun box(): String {
    val values = MySet<Int, Int, Map.Entry<Int, Int>>()
    val found = (object {}).let { values.contains(it as Any?) }
    return if (found) "NOT OK" else "OK"
}
"#;
    assert_eq!(run(SRC).expect("AbstractSet contains barrier"), "OK");
}

/// A delegated `Map<Wrapper, String>` stores boxed `Wrapper` keys. `get` must accept that box and
/// reject a foreign wrapper with null.
#[test]
fn value_class_map_delegation_accepts_its_own_key() {
    const SRC: &str = r#"
@JvmInline
value class Wrapper(val id: Int)

@JvmInline
value class GenericWrapper<T : Int>(val id: T)

class DMap(private val map: Map<Wrapper, String>) : Map<Wrapper, String> by map

class GenericDMap(private val map: Map<GenericWrapper<Int>, String>) :
    Map<GenericWrapper<Int>, String> by map

fun box(): String {
    val values = DMap(mutableMapOf(Wrapper(42) to "OK"))
    if (values[Wrapper(42)] != "OK") return "miss"
    val erased = values as Map<Any?, String>
    if (erased[1] != null) return "foreign"

    val generic = GenericDMap(mutableMapOf(GenericWrapper(42) to "OK"))
    if (generic[GenericWrapper(42)] != "OK") return "generic miss"
    val genericErased = generic as Map<Any?, String>
    if (genericErased[1] != null) return "generic foreign"
    return "OK"
}
"#;
    assert_eq!(run(SRC).expect("value-class map delegation"), "OK");
}

/// `List<Nothing>` erases `contains`/`indexOf`/`lastIndexOf` to `Object`, while the override's
/// parameter is `java/lang/Void`. A value that is not `Void` — including `null` — is absent.
#[test]
fn nothing_list_bridges_report_absence_instead_of_casting_to_void() {
    const SRC: &str = r#"
private object EmptyList : List<Nothing> {
    override fun contains(element: Nothing): Boolean = false
    override fun containsAll(elements: Collection<Nothing>): Boolean = elements.isEmpty()
    override fun indexOf(element: Nothing): Int = -2
    override fun lastIndexOf(element: Nothing): Int = -2
    override val size: Int get() = 0
    override fun isEmpty(): Boolean = true
    override fun iterator(): Iterator<Nothing> = throw UnsupportedOperationException()
    override fun get(index: Int): Nothing = throw UnsupportedOperationException()
    override fun listIterator(): ListIterator<Nothing> = throw UnsupportedOperationException()
    override fun listIterator(index: Int): ListIterator<Nothing> = throw UnsupportedOperationException()
    override fun subList(fromIndex: Int, toIndex: Int): List<Nothing> = throw UnsupportedOperationException()
}

fun box(): String {
    val n = EmptyList as List<String>
    if (n.contains("")) return "fail 1"
    if (n.indexOf("") != -1) return "fail 2"
    if (n.lastIndexOf("") != -1) return "fail 3"

    val nullAny = EmptyList as List<Any?>
    if (nullAny.contains(null)) return "fail 4"
    if (nullAny.indexOf(null) != -1) return "fail 5"
    if (nullAny.lastIndexOf(null) != -1) return "fail 6"
    return "OK"
}
"#;
    let krusty = run(SRC).expect("List<Nothing> bridges");
    assert_eq!(krusty, "OK");
    assert_eq!(common::kotlinc_box_result(SRC), krusty);
}

/// `Map.containsValue` is the value-side twin of `contains`: a non-`Void` argument is not a value
/// of a `Nothing` map.
#[test]
fn nothing_map_contains_value_reports_absence() {
    const SRC: &str = r#"
private object EmptyMap : Map<Any, Nothing> {
    override val size: Int get() = 0
    override fun isEmpty(): Boolean = true
    override fun containsKey(key: Any): Boolean = false
    override fun containsValue(value: Nothing): Boolean = false
    override fun get(key: Any): Nothing? = null
    override val entries: Set<Map.Entry<String, Nothing>> get() = null!!
    override val keys: Set<String> get() = null!!
    override val values: Collection<Nothing> get() = null!!
}

fun box(): String {
    val n = EmptyMap as Map<Any?, Any?>
    if (n.containsValue(null)) return "fail null"
    if (n.containsValue("")) return "fail string"
    return "OK"
}
"#;
    let krusty = run(SRC).expect("Map<_, Nothing>.containsValue");
    assert_eq!(krusty, "OK");
    assert_eq!(common::kotlinc_box_result(SRC), krusty);
}

/// `Nothing?` is a nullable `Void`. `null` is a value of that parameter and runs the override;
/// a non-null foreign value still misses `instanceof Void` and takes the neutral result.
#[test]
fn nullable_nothing_list_admits_null_and_rejects_a_foreign_value() {
    const SRC: &str = r#"
private object EmptyList : List<Nothing?> {
    override fun contains(element: Nothing?): Boolean = element == null
    override fun containsAll(elements: Collection<Nothing?>): Boolean = elements.isEmpty()
    override fun indexOf(element: Nothing?): Int = if (element == null) -2 else -3
    override fun lastIndexOf(element: Nothing?): Int = if (element == null) -2 else -3
    override val size: Int get() = 0
    override fun isEmpty(): Boolean = true
    override fun iterator(): Iterator<Nothing?> = throw UnsupportedOperationException()
    override fun get(index: Int): Nothing? = throw UnsupportedOperationException()
    override fun listIterator(): ListIterator<Nothing?> = throw UnsupportedOperationException()
    override fun listIterator(index: Int): ListIterator<Nothing?> = throw UnsupportedOperationException()
    override fun subList(fromIndex: Int, toIndex: Int): List<Nothing?> = throw UnsupportedOperationException()
}

fun box(): String {
    val n = EmptyList as List<Any?>
    if (!n.contains(null)) return "null contains"
    if (n.indexOf(null) != -2) return "null index ${n.indexOf(null)}"
    if (n.lastIndexOf(null) != -2) return "null last"
    if (n.contains("")) return "foreign contains"
    if (n.indexOf("") != -1) return "foreign index ${n.indexOf("")}"
    if (n.lastIndexOf("") != -1) return "foreign last"
    return "OK"
}
"#;
    let krusty = run(SRC).expect("List<Nothing?> bridges");
    assert_eq!(krusty, "OK");
    assert_eq!(common::kotlinc_box_result(SRC), krusty);
}
