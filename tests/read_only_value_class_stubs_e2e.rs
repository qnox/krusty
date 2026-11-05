//! A read-only collection face over value classes: kotlinc's value-class lowering replaces each
//! throwing stub whose signature mentions a value class, after `CollectionStubMethodLowering` has
//! written it. The stub takes the mangled name and declares every value-class position by its
//! carrier (`setValue-<hash>(I)I` for `Map.Entry<String, Z>`), and the mutable declaration's
//! erased descriptor keeps a throwing bridge, now the only method under its JVM name. A value
//! class's stubs are method replacements too, written before its representation members, except
//! `remove(Int)`, which keeps its name.
//!
//! A type-safe stub renamed by its value-class result (`remove-<hash>(Object)String` for
//! `Map<String, S?>`) leaves `remove(Object)` to kotlinc's special bridges: a final synthetic
//! `Object remove(Object)` that boxes the result, then a final `String remove(Object)`. A member
//! that a value class renames gets the same pair for `get(Object)`.
//!
//! The `Map` faces also carry the JDK's own `java.util.Map` defaults (`putIfAbsent`, ...), which
//! are a later realization; their comparison leaves those out by their exact spelling and compares
//! every other member byte for byte.
//!
//! A value class over `Collection<Z>` writes its interface entries by the overridden collection
//! declaration's signature policy (`containsAll(Collection<?>)`) and bridges `size()` as an
//! ordinary synthetic override; its `removeIf` default is the JDK's later realization. A class
//! extending a Kotlin `AbstractList`/`AbstractSet` gets the special bridges of the fake overrides
//! it inherits, and a value-class box passed to a declared parameter that names a type parameter
//! without being one (`Iterable<T>`, `Tagged<out T>`) is cast to it, as kotlinc's implicit cast is.

use super::common;

const ENTRIES: &str = "@JvmInline value class Z(val x: Int)
@JvmInline value class S(val s: String)

class ZEntry(override val key: String, override val value: Z) : Map.Entry<String, Z>

class SEntry(override val key: S, override val value: S?) : Map.Entry<S, S?>

class Cursor(private val items: IntArray) : ListIterator<Z> {
    private var at = 0
    override fun hasNext() = at < items.size
    override fun next() = Z(items[at++])
    override fun nextIndex() = at
    override fun hasPrevious() = at > 0
    override fun previous() = Z(items[--at])
    override fun previousIndex() = at - 1
}

@JvmInline value class Names(private val name: String) : ListIterator<String> {
    override fun hasNext() = false
    override fun next() = name
    override fun nextIndex() = 0
    override fun hasPrevious() = false
    override fun previous() = name
    override fun previousIndex() = -1
}

/// `name` called through the JDK interface `face`, as a Java caller reaches it.
fun through(face: String, receiver: Any, name: String, vararg arguments: Any?): String = try {
    val method = Class.forName(face).methods.single { it.name == name && it.parameterCount == arguments.size }
    \"$name=${method.invoke(receiver, *arguments)}\"
} catch (e: java.lang.reflect.InvocationTargetException) {
    \"$name: ${e.cause!!.javaClass.simpleName}\"
}

fun box(): String {
    val entry = \"java.util.Map\\$Entry\"
    val iterator = \"java.util.ListIterator\"
    val outcomes = listOf(
        through(entry, ZEntry(\"k\", Z(1)), \"setValue\", Z(2)),
        through(entry, SEntry(S(\"k\"), null), \"setValue\", S(\"v\")),
        through(entry, ZEntry(\"k\", Z(1)), \"getValue\"),
        through(iterator, Cursor(intArrayOf(1, 2)), \"set\", Z(3)),
        through(iterator, Cursor(intArrayOf(1, 2)), \"add\", Z(3)),
        through(iterator, Cursor(intArrayOf(1, 2)), \"next\"),
        through(iterator, Names(\"n\"), \"add\", \"m\"),
        through(iterator, Names(\"n\"), \"next\"),
    ).joinToString(\"; \")
    val expected = \"setValue: UnsupportedOperationException; setValue: UnsupportedOperationException; \" +
        \"getValue=Z(x=1); set: UnsupportedOperationException; add: UnsupportedOperationException; \" +
        \"next=Z(x=1); add: UnsupportedOperationException; next=n\"
    return if (outcomes == expected) \"OK\" else outcomes
}
";

/// `source` in the package the class comparisons name; a `box()` fixture stays in the root package.
fn packaged(source: &str) -> String {
    format!("package stubs\n\n{source}")
}

#[test]
fn a_map_entry_stub_takes_its_value_class_mangling_like_kotlinc() {
    common::assert_class_matches_kotlinc("ValueEntryStubs", &packaged(ENTRIES), "stubs/ZEntry");
    common::assert_class_matches_kotlinc("ValueEntryStubs", &packaged(ENTRIES), "stubs/SEntry");
}

#[test]
fn a_list_iterator_stub_takes_its_value_class_mangling_like_kotlinc() {
    common::assert_class_matches_kotlinc("ValueIteratorStubs", &packaged(ENTRIES), "stubs/Cursor");
}

#[test]
fn a_value_class_writes_its_mangled_stubs_before_its_representation_like_kotlinc() {
    common::assert_class_matches_kotlinc("ValueClassStubs", &packaged(ENTRIES), "stubs/Names");
}

#[test]
fn value_class_stubs_throw_and_bridge_like_kotlinc() {
    common::expect_box_same_as_kotlinc(ENTRIES, "read_only_value_class_stubs");
}

const MAPS: &str = "@JvmInline value class Z(val x: Int)
@JvmInline value class S(val s: String)

class ByName : Map<String, S?> {
    override val size = 1
    override fun isEmpty() = false
    override fun containsKey(key: String) = key == \"k\"
    override fun containsValue(value: S?) = value == S(\"v\")
    override fun get(key: String): S? = if (key == \"k\") S(\"v\") else null
    override val keys: Set<String> = setOf(\"k\")
    override val values: Collection<S?> = listOf(S(\"v\"))
    override val entries: Set<Map.Entry<String, S?>> = emptySet()
}

class ByKey : Map<Z, String> {
    override val size = 1
    override fun isEmpty() = false
    override fun containsKey(key: Z) = key == Z(1)
    override fun containsValue(value: String) = value == \"v\"
    override fun get(key: Z): String? = if (key == Z(1)) \"v\" else null
    override val keys: Set<Z> = setOf(Z(1))
    override val values: Collection<String> = listOf(\"v\")
    override val entries: Set<Map.Entry<Z, String>> = emptySet()
}

/// `name` called through `java.util.Map`, as a Java caller reaches it.
fun through(receiver: Any, name: String, vararg arguments: Any?): String = try {
    val method = Class.forName(\"java.util.Map\").methods.single {
        it.name == name && it.parameterCount == arguments.size
    }
    \"$name=${method.invoke(receiver, *arguments)}\"
} catch (e: java.lang.reflect.InvocationTargetException) {
    \"$name: ${e.cause!!.javaClass.simpleName}\"
}

fun box(): String {
    val outcomes = listOf(
        through(ByName(), \"get\", \"k\"),
        through(ByName(), \"get\", \"x\"),
        through(ByName(), \"get\", 1),
        through(ByName(), \"remove\", \"k\"),
        through(ByName(), \"put\", \"k\", S(\"w\")),
        through(ByKey(), \"get\", Z(1)),
        through(ByKey(), \"get\", \"x\"),
        through(ByKey(), \"remove\", Z(1)),
        through(ByKey(), \"put\", Z(2), \"w\"),
    ).joinToString(\"; \")
    val expected = \"get=S(s=v); get=null; get=null; remove: UnsupportedOperationException; \" +
        \"put: UnsupportedOperationException; get=v; get=null; \" +
        \"remove: UnsupportedOperationException; put: UnsupportedOperationException\"
    return if (outcomes == expected) \"OK\" else outcomes
}
";

/// The members of `java.util.Map` a later realization writes, as `javap -p` spells them.
const LATER_REALIZATIONS: &[&str] = &[
    "public void replaceAll(java.util.function.BiFunction<? super java.lang.String, ? super stubs.S, ? extends stubs.S>);",
    "public java.lang.String putIfAbsent-",
    "public boolean replace-",
    "public java.lang.String replace-",
    "public java.lang.String computeIfAbsent-",
    "public java.lang.String computeIfPresent-",
    "public java.lang.String compute-",
    "public java.lang.String merge-",
    "public void replaceAll(java.util.function.BiFunction<? super stubs.Z, ? super java.lang.String, ? extends java.lang.String>);",
    "public java.lang.Object putIfAbsent(java.lang.Object, java.lang.Object);",
    "public boolean replace(java.lang.Object, java.lang.Object, java.lang.Object);",
    "public java.lang.Object replace(java.lang.Object, java.lang.Object);",
    "public java.lang.Object computeIfAbsent(java.lang.Object, java.util.function.Function);",
    "public java.lang.Object computeIfPresent(java.lang.Object, java.util.function.BiFunction);",
    "public java.lang.Object compute(java.lang.Object, java.util.function.BiFunction);",
    "public java.lang.Object merge(java.lang.Object, java.lang.Object, java.util.function.BiFunction);",
];

fn assert_map_matches_kotlinc(class: &str, members: &[&str]) {
    assert_members_match_kotlinc(("Maps.kt", MAPS), class, LATER_REALIZATIONS, members);
}

/// `class`'s declarations, except the `later` realizations, and the code of each of `members`
/// match kotlinc's. A `later` entry ending in `-` names every mangled spelling it begins.
fn assert_members_match_kotlinc(
    (file, source): (&str, &str),
    class: &str,
    later: &[&str],
    members: &[&str],
) {
    let classes = common::classes_against_kotlinc_module(&[(file, &packaged(source))]);
    let (reference, krusty) = classes
        .method_declarations(class)
        .unwrap_or_else(|| panic!("both compilers write {class}"));
    let realized_here = |declarations: Vec<String>| {
        declarations
            .into_iter()
            .filter(|declaration| {
                !later.iter().any(|later| {
                    declaration.as_str() == *later
                        || later.ends_with('-') && declaration.starts_with(later)
                })
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(realized_here(krusty), realized_here(reference));
    let (reference, krusty) = classes.method_blocks(class);
    for member in members {
        let block = |blocks: &[(String, String)]| {
            blocks
                .iter()
                .filter(|(declaration, _)| declaration == member)
                .map(|(_, body)| body.clone())
                .collect::<Vec<_>>()
        };
        assert!(
            !block(&reference).is_empty(),
            "{class} declares no `{member}`"
        );
        assert_eq!(block(&krusty), block(&reference), "{class}: `{member}`");
    }
}

#[test]
fn a_type_safe_stub_renamed_by_its_value_class_result_gets_kotlincs_special_bridges() {
    assert_map_matches_kotlinc(
        "stubs/ByName",
        &[
            "public java.lang.String put-VsWcXTM(java.lang.String, java.lang.String);",
            "public java.lang.String remove-1QzqJSA(java.lang.Object);",
            "public final java.lang.Object remove(java.lang.Object);",
            "public final java.lang.String remove(java.lang.Object);",
            "public final java.lang.Object get(java.lang.Object);",
            "public final java.lang.String get(java.lang.Object);",
            "public java.lang.Object put(java.lang.Object, java.lang.Object);",
        ],
    );
}

#[test]
fn a_member_renamed_by_its_value_class_key_gets_kotlincs_special_get_bridges() {
    assert_map_matches_kotlinc(
        "stubs/ByKey",
        &[
            "public java.lang.String put-4urwq08(int, java.lang.String);",
            "public java.lang.String remove(java.lang.Object);",
            "public final java.lang.Object get(java.lang.Object);",
            "public final java.lang.String get(java.lang.Object);",
            "public java.lang.Object put(java.lang.Object, java.lang.Object);",
            "public java.lang.Object remove(java.lang.Object);",
        ],
    );
}

#[test]
fn value_class_map_bridges_answer_like_kotlinc() {
    common::expect_box_same_as_kotlinc(MAPS, "read_only_value_class_map_stubs");
}

const COLLECTIONS: &str = "interface Tagged<T>

@JvmInline value class Z(val x: Int) : Tagged<String>

class Words(private val words: Array<String>) : AbstractList<String>() {
    override val size get() = words.size
    override fun get(index: Int) = words[index]
}

class Pairs(private val count: Int) : AbstractSet<Map.Entry<String, Z>>() {
    override val size get() = count
    override fun iterator(): Iterator<Map.Entry<String, Z>> = emptyList<Map.Entry<String, Z>>().iterator()
}

@JvmInline value class Zs(private val xs: IntArray) : Collection<Z> {
    override val size get() = xs.size
    override fun isEmpty() = xs.isEmpty()
    override fun iterator(): Iterator<Z> = xs.map(::Z).iterator()
    override fun contains(element: Z) = element.x in xs
    override fun containsAll(elements: Collection<Z>) = elements.all { contains(it) }
}

fun <T> Iterable<T>.holds(element: T) = contains(element)

fun held(zs: Zs, z: Z) = zs.holds(z)

fun position(zs: Zs, z: Z) = zs.indexOf(z)

fun <T> tagOf(tagged: Tagged<out T>) = tagged.toString()

fun <T : Tagged<String>> bound(tagged: T) = tagged.toString()

fun tags(z: Z) = tagOf(z) + bound(z)

/// The Java caller, kept off the file facade the class comparison names.
object Java {
    /// `name` called through the JDK interface `face`.
    fun through(face: String, receiver: Any, name: String, vararg arguments: Any?): String {
        val method = Class.forName(face).methods.single { it.name == name && it.parameterCount == arguments.size }
        return \"$name=${method.invoke(receiver, *arguments)}\"
    }
}

fun box(): String {
    val collection = \"java.util.Collection\"
    val words = Words(arrayOf(\"a\", \"b\"))
    val zs = Zs(intArrayOf(1, 2))
    val outcomes = listOf(
        Java.through(collection, words, \"contains\", \"b\"),
        Java.through(collection, words, \"contains\", 1),
        Java.through(\"java.util.List\", words, \"indexOf\", \"b\"),
        Java.through(\"java.util.List\", words, \"lastIndexOf\", 1),
        Java.through(collection, Pairs(0), \"contains\", \"x\"),
        Java.through(collection, zs, \"contains\", Z(2)),
        Java.through(collection, zs, \"contains\", 2),
        Java.through(collection, zs, \"containsAll\", listOf(Z(1))),
        Java.through(collection, zs, \"size\"),
        \"held=${held(zs, Z(1))}\",
        \"position=${position(zs, Z(2))}\",
        tags(Z(3)),
    ).joinToString(\"; \")
    val expected = \"contains=true; contains=false; indexOf=1; lastIndexOf=-1; \" +
        \"contains=false; contains=true; contains=false; containsAll=true; size=2; held=true; position=1; Z(x=3)Z(x=3)\"
    return if (outcomes == expected) \"OK\" else outcomes
}
";

/// The members of `java.util.Collection` a later realization writes, as `javap -p` spells them.
const COLLECTION_LATER_REALIZATIONS: &[&str] =
    &["public boolean removeIf(java.util.function.Predicate<? super stubs.Z>);"];

#[test]
fn a_value_class_collection_maps_its_special_members_like_kotlinc() {
    assert_members_match_kotlinc(
        ("ValueCollections.kt", COLLECTIONS),
        "stubs/Zs",
        COLLECTION_LATER_REALIZATIONS,
        &[
            "public final boolean contains(java.lang.Object);",
            "public boolean containsAll(java.util.Collection<?>);",
            "public int size();",
        ],
    );
}

#[test]
fn value_class_collection_members_answer_like_kotlinc() {
    common::expect_box_same_as_kotlinc(COLLECTIONS, "read_only_value_class_collections");
}
