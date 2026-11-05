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
//! The `Map` faces also carry the JDK's own `java.util.Map` defaults (`putIfAbsent`, ...), whose
//! positions take kotlinc's predefined enhancement before the face's arguments: `merge` of a
//! `Map<String, S?>` takes the value as `S` and returns `S?` (`merge--vSzEUQ`), while
//! `putIfAbsent` of a `Map<Z, String>` keeps the declared key `Z` (`putIfAbsent-4urwq08(int,
//! String)`). Every member is compared byte for byte.

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
        through(ByName(), \"putIfAbsent\", \"k\", S(\"w\")),
        through(ByName(), \"merge\", \"k\", S(\"w\"), java.util.function.BiFunction<Any?, Any?, Any?> { _, w -> w }),
        through(ByKey(), \"putIfAbsent\", Z(2), \"w\"),
        through(ByKey(), \"computeIfAbsent\", Z(2), java.util.function.Function<Any?, Any?> { \"w\" }),
    ).joinToString(\"; \")
    val expected = \"get=S(s=v); get=null; get=null; remove: UnsupportedOperationException; \" +
        \"put: UnsupportedOperationException; get=v; get=null; \" +
        \"remove: UnsupportedOperationException; put: UnsupportedOperationException; \" +
        \"putIfAbsent: UnsupportedOperationException; merge: UnsupportedOperationException; \" +
        \"putIfAbsent: UnsupportedOperationException; computeIfAbsent: UnsupportedOperationException\"
    return if (outcomes == expected) \"OK\" else outcomes
}
";

fn assert_map_matches_kotlinc(class: &str, members: &[&str]) {
    let classes = common::classes_against_kotlinc_module(&[("Maps.kt", &packaged(MAPS))]);
    let (reference, krusty) = classes
        .method_declarations(class)
        .unwrap_or_else(|| panic!("both compilers write {class}"));
    assert_eq!(krusty, reference);
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
            "public void replaceAll(java.util.function.BiFunction<? super java.lang.String, ? super stubs.S, ? extends stubs.S>);",
            "public java.lang.String putIfAbsent-VsWcXTM(java.lang.String, java.lang.String);",
            "public boolean replace-OH16CYI(java.lang.String, java.lang.String, java.lang.String);",
            "public java.lang.String replace-VsWcXTM(java.lang.String, java.lang.String);",
            "public java.lang.String computeIfAbsent-cKo1FWk(java.lang.String, java.util.function.Function<? super java.lang.String, ? extends stubs.S>);",
            "public java.lang.String computeIfPresent-cKo1FWk(java.lang.String, java.util.function.BiFunction<? super java.lang.String, ? super stubs.S, ? extends stubs.S>);",
            "public java.lang.String compute-cKo1FWk(java.lang.String, java.util.function.BiFunction<? super java.lang.String, ? super stubs.S, ? extends stubs.S>);",
            "public java.lang.String merge--vSzEUQ(java.lang.String, java.lang.String, java.util.function.BiFunction<? super stubs.S, ? super stubs.S, ? extends stubs.S>);",
            "public java.lang.Object putIfAbsent(java.lang.Object, java.lang.Object);",
            "public boolean replace(java.lang.Object, java.lang.Object, java.lang.Object);",
            "public java.lang.Object replace(java.lang.Object, java.lang.Object);",
            "public java.lang.Object computeIfAbsent(java.lang.Object, java.util.function.Function);",
            "public java.lang.Object computeIfPresent(java.lang.Object, java.util.function.BiFunction);",
            "public java.lang.Object compute(java.lang.Object, java.util.function.BiFunction);",
            "public java.lang.Object merge(java.lang.Object, java.lang.Object, java.util.function.BiFunction);",
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
            "public void replaceAll(java.util.function.BiFunction<? super stubs.Z, ? super java.lang.String, ? extends java.lang.String>);",
            "public java.lang.String putIfAbsent-4urwq08(int, java.lang.String);",
            "public boolean replace-OX4Q0o0(int, java.lang.String, java.lang.String);",
            "public java.lang.String replace-4urwq08(int, java.lang.String);",
            "public java.lang.String computeIfAbsent-4urwq08(int, java.util.function.Function<? super stubs.Z, ? extends java.lang.String>);",
            "public java.lang.String computeIfPresent-4urwq08(int, java.util.function.BiFunction<? super stubs.Z, ? super java.lang.String, ? extends java.lang.String>);",
            "public java.lang.String compute-4urwq08(int, java.util.function.BiFunction<? super stubs.Z, ? super java.lang.String, ? extends java.lang.String>);",
            "public java.lang.String merge-OX4Q0o0(int, java.lang.String, java.util.function.BiFunction<? super java.lang.String, ? super java.lang.String, ? extends java.lang.String>);",
            "public java.lang.Object putIfAbsent(java.lang.Object, java.lang.Object);",
            "public boolean replace(java.lang.Object, java.lang.Object, java.lang.Object);",
            "public java.lang.Object replace(java.lang.Object, java.lang.Object);",
            "public java.lang.Object computeIfAbsent(java.lang.Object, java.util.function.Function);",
            "public java.lang.Object computeIfPresent(java.lang.Object, java.util.function.BiFunction);",
            "public java.lang.Object compute(java.lang.Object, java.util.function.BiFunction);",
            "public java.lang.Object merge(java.lang.Object, java.lang.Object, java.util.function.BiFunction);",
        ],
    );
}

#[test]
fn value_class_map_bridges_answer_like_kotlinc() {
    common::expect_box_same_as_kotlinc(MAPS, "read_only_value_class_map_stubs");
}

/// The JDK defaults over every kind of argument: a non-null value class takes its carrier
/// (`computeIfAbsent-4G9NThA(int, Function)int` returns the declared `V`), a nullable result is the
/// box (`putIfAbsent-kQKnxJI(int, int)Z` returns `V?`), a nullable key stays the box, and a scalar
/// in the JDK's reference slot is its wrapper (`addFirst(Integer)`, `Integer removeFirst()`), which
/// leaves the hash to the semantic type. Each class header and every member, code included, is
/// kotlinc's.
const REFERENCE_SLOTS: &str = "@JvmInline value class Z(val x: Int)

class Both(map: Map<Z, Z>) : Map<Z, Z> by map
class Counts(map: Map<Z, Int>) : Map<Z, Int> by map
class Optional(map: Map<Z?, String>) : Map<Z?, String> by map
class Ints(private val items: List<Int>) : List<Int> {
    override val size get() = items.size
    override fun isEmpty() = items.isEmpty()
    override fun contains(element: Int) = items.contains(element)
    override fun containsAll(elements: Collection<Int>) = items.containsAll(elements)
    override fun get(index: Int) = items[index]
    override fun indexOf(element: Int) = items.indexOf(element)
    override fun lastIndexOf(element: Int) = items.lastIndexOf(element)
    override fun iterator() = items.iterator()
    override fun listIterator() = items.listIterator()
    override fun listIterator(index: Int) = items.listIterator(index)
    override fun subList(fromIndex: Int, toIndex: Int) = items.subList(fromIndex, toIndex)
}
class Zs(private val items: List<Z>) : List<Z> {
    override val size get() = items.size
    override fun isEmpty() = items.isEmpty()
    override fun contains(element: Z) = items.contains(element)
    override fun containsAll(elements: Collection<Z>) = items.containsAll(elements)
    override fun get(index: Int) = items[index]
    override fun indexOf(element: Z) = items.indexOf(element)
    override fun lastIndexOf(element: Z) = items.lastIndexOf(element)
    override fun iterator() = items.iterator()
    override fun listIterator() = items.listIterator()
    override fun listIterator(index: Int) = items.listIterator(index)
    override fun subList(fromIndex: Int, toIndex: Int) = items.subList(fromIndex, toIndex)
}
class Bag(private val items: Collection<Z>) : Collection<Z> {
    override val size get() = items.size
    override fun isEmpty() = items.isEmpty()
    override fun contains(element: Z) = items.contains(element)
    override fun containsAll(elements: Collection<Z>) = items.containsAll(elements)
    override fun iterator() = items.iterator()
}
";

#[test]
fn jdk_defaults_take_kotlincs_enhanced_value_class_slots() {
    let source = packaged(REFERENCE_SLOTS);
    for class in [
        "stubs/Both",
        "stubs/Counts",
        "stubs/Optional",
        "stubs/Ints",
        "stubs/Zs",
        "stubs/Bag",
    ] {
        common::assert_class_code_matches_kotlinc("ReferenceSlotStubs", &source, class);
    }
}
