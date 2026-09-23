//! The SPECIAL BRIDGES Kotlin puts in front of a collection member a program implements itself.
//!
//! `Collection<E>.contains(x as E)` erases to a call that takes anything, and `EmptyMap.get(null)`
//! reaches a `get(key: Any)`. Kotlin answers such a call WITHOUT running the override: the bridge
//! tests the argument against what the override declares and hands back the member's own default
//! when it is not one — `false` for the questions, `-1` for the positions, `null` for the lookups,
//! and the caller's own fallback for `getOrDefault`.
//!
//! This is why these members used to decline outright for a file that declares its own collection:
//! the dispatch among the file's classes had no bridge to put in front of each arm. The default is
//! now read off the member's ANSWER rather than named per member, because the two must agree —
//! `Map.remove` answers the value it removed while `MutableCollection.remove` answers a boolean,
//! and a per-name table got that pair wrong in exactly the way the verifier catches.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// Require kotlinc's answer and the NATIVE answer to agree, where krusty's JVM backend does not
/// emit the bridge METHOD and so cannot meet the expectation yet.
///
/// The two that need this declare a map whose members take a NON-NULL parameter. On the JVM a
/// bridge is a real method — `get(Object)` beside `get(Any)` — and without one the call either
/// finds no implementation at all (`AbstractMethodError`) or reaches the override and trips its
/// own null check. That is a gap on that side, unchanged by anything here: stripping every
/// `src/native/` change from the tree leaves the same failure. Move these to
/// [`every_backend_agrees_with_kotlinc`] once the JVM backend emits the bridges.
fn kotlinc_and_native_agree(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_native_box(source, stem, "OK");
}

/// `contains` reached through the erased call: the override runs only for an argument it accepts.
#[test]
fn a_collections_contains_is_bridged_by_the_argument() {
    let source = r#"
class StrList : List<String?> {
    override val size: Int get() = throw UnsupportedOperationException()
    override fun isEmpty(): Boolean = throw UnsupportedOperationException()
    override fun contains(o: String?) = o == null || o == "abc"
    override fun iterator(): Iterator<String> = throw UnsupportedOperationException()
    override fun containsAll(c: Collection<String?>) = false
    override fun get(index: Int): String = throw UnsupportedOperationException()
    override fun indexOf(o: String?): Int = throw UnsupportedOperationException()
    override fun lastIndexOf(o: String?): Int = throw UnsupportedOperationException()
    override fun listIterator(): ListIterator<String?> = throw UnsupportedOperationException()
    override fun listIterator(index: Int): ListIterator<String?> =
        throw UnsupportedOperationException()
    override fun subList(fromIndex: Int, toIndex: Int): List<String?> =
        throw UnsupportedOperationException()
}

fun <E> Collection<E>.forceContains(x: Any?): Boolean = contains(x as E)

fun box(): String {
    val strList = StrList()
    // An `Int` is not a `String?`, so the bridge answers false and the override never runs —
    // which matters here, because every other member of this class throws.
    if (strList.forceContains(1)) return "fail int"
    if (!strList.forceContains(null)) return "fail null"
    if (strList.forceContains("cde")) return "fail miss"
    if (!strList.forceContains("abc")) return "fail hit"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_bridge_contains", source);
}

/// A map's four bridged members, each with its own default: `null` for the two that answer a value
/// and `false` for the two that answer a question.
#[test]
fn a_maps_lookups_are_bridged_by_the_key() {
    let source = r#"
private object NotEmptyMap : MutableMap<Any, Any> {
    override fun containsKey(key: Any): Boolean = true
    override fun containsValue(value: Any): Boolean = true
    override fun get(key: Any): Any? = "here"
    override fun remove(key: Any): Any? = "gone"

    override val size: Int get() = 0
    override fun isEmpty(): Boolean = true
    override fun put(key: Any, value: Any): Any? = throw UnsupportedOperationException()
    override fun putAll(from: Map<out Any, Any>): Unit = throw UnsupportedOperationException()
    override fun clear(): Unit = throw UnsupportedOperationException()
    override val entries: MutableSet<MutableMap.MutableEntry<Any, Any>> get() = null!!
    override val keys: MutableSet<Any> get() = null!!
    override val values: MutableCollection<Any> get() = null!!
}

fun box(): String {
    val n = NotEmptyMap as MutableMap<Any?, Any?>
    // The declarations take a non-null `Any`, so `null` is not one of theirs.
    if (n.get(null) != null) return "fail get null"
    if (n.containsKey(null)) return "fail containsKey null"
    if (n.containsValue(null)) return "fail containsValue null"
    if (n.remove(null) != null) return "fail remove null"
    // A non-null key IS one of theirs, so each override runs.
    if (n.get("") != "here") return "fail get"
    if (!n.containsKey("")) return "fail containsKey"
    if (!n.containsValue("")) return "fail containsValue"
    if (n.remove("") != "gone") return "fail remove"
    return "OK"
}
"#;
    kotlinc_and_native_agree("native_bridge_map", source);
}

/// A parameter typed `Nothing` admits nothing at all, so its bridge is the constant answer.
#[test]
fn a_nothing_parameter_admits_nothing() {
    let source = r#"
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
    if (n.get(null) != null) return "fail get"
    if (n.containsKey(null)) return "fail containsKey"
    if (n.containsValue(null)) return "fail containsValue null"
    if (n.containsValue("x")) return "fail containsValue text"
    return "OK"
}
"#;
    kotlinc_and_native_agree("native_bridge_nothing", source);
}

/// `indexOf`/`lastIndexOf` answer `-1` rather than `false`, which is the reason the default is read
/// off the member's answer instead of being named beside it.
#[test]
fn a_bridged_position_answers_minus_one() {
    let source = r#"
class Only(private val kept: String) : List<String> {
    override val size: Int get() = 1
    override fun isEmpty(): Boolean = false
    override fun contains(element: String): Boolean = element == kept
    override fun indexOf(element: String): Int = if (element == kept) 0 else -1
    override fun lastIndexOf(element: String): Int = indexOf(element)
    override fun get(index: Int): String = kept
    override fun containsAll(elements: Collection<String>): Boolean = false
    override fun iterator(): Iterator<String> = throw UnsupportedOperationException()
    override fun listIterator(): ListIterator<String> = throw UnsupportedOperationException()
    override fun listIterator(index: Int): ListIterator<String> =
        throw UnsupportedOperationException()
    override fun subList(fromIndex: Int, toIndex: Int): List<String> =
        throw UnsupportedOperationException()
}

fun <E> List<E>.forceIndexOf(x: Any?): Int = indexOf(x as E)
fun <E> List<E>.forceLastIndexOf(x: Any?): Int = lastIndexOf(x as E)

fun box(): String {
    val only = Only("abc")
    if (only.forceIndexOf(1) != -1) return "fail indexOf int"
    if (only.forceLastIndexOf(1) != -1) return "fail lastIndexOf int"
    if (only.forceIndexOf("abc") != 0) return "fail indexOf hit"
    if (only.forceIndexOf("z") != -1) return "fail indexOf miss"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_bridge_position", source);
}

/// An override whose parameter is a SCALAR is still reachable through the erased call.
///
/// `Map<String, Int>.containsValue(value: V)` states a reference and the override takes an
/// unboxed `Int`; the bridge tests the box's own descriptor and the arm unboxes it. The reverse
/// never happens — a call stating a scalar has nothing to widen from — and telling the two
/// directions apart is what keeps `removeAt`, which a JVM realization spells `remove(int)`, from
/// binding to a class's own `remove(String)`.
#[test]
fn a_scalar_override_is_reached_by_unboxing() {
    let source = r#"
class Counts : MutableMap<String, Int> {
    override fun containsValue(value: Int): Boolean = value == 3
    override fun containsKey(key: String): Boolean = key == "k"
    override fun get(key: String): Int? = if (key == "k") 3 else null

    override val size: Int get() = 1
    override fun isEmpty(): Boolean = false
    override fun put(key: String, value: Int): Int? = throw UnsupportedOperationException()
    override fun putAll(from: Map<out String, Int>): Unit = throw UnsupportedOperationException()
    override fun remove(key: String): Int? = throw UnsupportedOperationException()
    override fun clear(): Unit = throw UnsupportedOperationException()
    override val entries: MutableSet<MutableMap.MutableEntry<String, Int>> get() = null!!
    override val keys: MutableSet<String> get() = null!!
    override val values: MutableCollection<Int> get() = null!!
}

fun box(): String {
    val erased = Counts() as MutableMap<Any?, Any?>
    // A `String` is not an `Int`, so the bridge answers false without unboxing anything.
    if (erased.containsValue("k")) return "fail wrong type"
    if (erased.containsValue(null)) return "fail null"
    // An `Int` box is one, so the override runs on the unboxed value.
    if (!erased.containsValue(3)) return "fail hit"
    if (erased.containsValue(4)) return "fail miss"
    // The key side is the ordinary reference case, in the same class.
    if (!erased.containsKey("k")) return "fail key"
    if (erased.containsKey(1)) return "fail key type"
    return "OK"
}
"#;
    kotlinc_and_native_agree("native_bridge_scalar_override", source);
}
