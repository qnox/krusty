//! kotlinc's `EnhancedNullability` attribute travels with the type it marks. FIR enhances a Java
//! result from the declarations it overrides (`computeIndexedQualifiers`): `HashMap.entrySet()`,
//! read as `entries`, is a set of marked entries of marked `K` and `V`. Substitution keeps an
//! argument's marks, so a value derived from such a result through a type parameter — an
//! iterator's `next()`, a generic extension's result, an entry's `key`, a loop variable — is a
//! marked Java value nothing checked, and `insertSpecialCast` guards it where its expected type
//! rejects `null`. A local inferred from a marked value keeps its argument marks but not its head;
//! a type variable fixed from several values is marked only where all of them are. A value passed
//! to a parameter whose unsubstituted type is a nullable-bounded type parameter is not guarded. A
//! JDK member of a mapped builtin classifier is the builtin's declaration and carries no mark.

use super::common;

const SOURCE: &str = r#"
import java.util.ArrayList
import java.util.HashMap
import java.util.HashSet
import java.util.TreeMap

fun <T> Iterable<T>.firstOf(): T = iterator().next()

fun <T> pick(first: T, second: T): T = first

fun <T> withDefault(value: T, ignored: Int = 0): T = value

fun <T> firstVararg(vararg values: T): T = values[0]

fun <A, B> A.pairedWith(other: B): String = "" + this + other

class Box<T>(val value: T) {
    fun get(): T = value
}

fun firstKey(map: HashMap<String, String>): String = map.entries.firstOf().key

fun firstEntry(map: HashMap<String, String>): Map.Entry<String, String> = map.entries.firstOf()

fun firstValue(map: HashMap<String, Int>): Int = map.values.firstOf() + 1

fun nextKey(map: HashMap<String, String>): String = map.keys.iterator().next()

fun nextElement(set: HashSet<String>): String = set.iterator().next()

fun element(list: ArrayList<String>): String = list.get(0)

fun inferredKeys(map: HashMap<String, String>): Int {
    val keys = map.keys
    return keys.size
}

fun inferredEntries(map: HashMap<String, String>): String {
    val entries = map.entries
    val entry = entries.firstOf()
    return entry.key
}

fun declaredEntries(map: HashMap<String, String>): String {
    val entries: Set<Map.Entry<String, String>> = map.entries
    return entries.firstOf().key
}

fun loopKey(map: HashMap<String, String>): String {
    for (entry in map.entries) return entry.key
    return ""
}

fun loopValue(map: HashMap<String, String>): String {
    val values = map.values
    for (value in values) {
        val copy = value
        return copy
    }
    return ""
}

fun pickedMixed(map: HashMap<String, String>, other: Set<String>): Int {
    val keys = pick(map.keys, other)
    return keys.size
}

fun pickedMarked(map: HashMap<String, String>): Int {
    val keys = pick(map.keys, map.keys)
    return keys.size
}

fun pickedNamed(map: HashMap<String, String>): Int {
    val keys = pick(second = map.keys, first = map.keys)
    return keys.size
}

fun defaulted(map: HashMap<String, String>): Int = withDefault(map.keys).size

fun oneVararg(map: HashMap<String, String>): Int = firstVararg(map.keys).size

fun manyVararg(map: HashMap<String, String>): Int = firstVararg(map.keys, map.keys).size

fun wholeVararg(map: HashMap<String, String>): Int {
    val values = arrayOf(map.keys)
    return firstVararg(values = values).size
}

fun boxed(map: HashMap<String, Any>): Any = Box(map.values.firstOf())

fun paired(map: HashMap<String, String>): String = "K".pairedWith(map.keys.firstOf())

fun boxedValue(map: HashMap<String, Any>): Any {
    val box = Box(map.values.firstOf())
    return box.get()
}

fun branchKey(map: HashMap<String, String>, first: Boolean): String =
    if (first) map.keys.firstOf() else ""

fun whenKey(map: HashMap<String, String>, first: Boolean): String = when {
    first -> map.keys.iterator().next()
    else -> ""
}

fun variableKey(map: HashMap<String, String>): String {
    var key = map.keys.firstOf()
    return key
}

fun treeValue(map: TreeMap<String, String>): String = map.entries.firstOf().value

fun nestedElement(map: HashMap<String, Iterable<String>>): String =
    map.values.firstOf().firstOf()

fun text(error: IllegalStateException, first: Boolean): String =
    if (first) error.toString() else ""

fun box(): String {
    val map = HashMap<String, String>()
    map.put("O", "K")
    val counts = HashMap<String, Int>()
    counts.put("O", 1)
    val set = HashSet<String>()
    set.add("O")
    val list = ArrayList<String>()
    list.add("O")
    val tree = TreeMap<String, String>()
    tree.put("O", "K")
    val nested = HashMap<String, Iterable<String>>()
    val values = HashMap<String, Any>()
    values.put("O", "K")
    nested.put("O", list)
    val result = firstKey(map) + firstEntry(map).value + firstValue(counts) + nextKey(map) +
        nextElement(set) + element(list) + inferredKeys(map) + inferredEntries(map) +
        declaredEntries(map) + loopKey(map) + loopValue(map) + pickedMixed(map, set) +
        pickedMarked(map) + pickedNamed(map) + defaulted(map) + oneVararg(map) +
        manyVararg(map) + wholeVararg(map) + (boxed(values) as Box<*>).get() + paired(map) +
        boxedValue(values) + branchKey(map, true) + whenKey(map, true) + variableKey(map) +
        treeValue(tree) + nestedElement(nested) + text(IllegalStateException(), false)
    return if (result == "OK2OOO1OOOK1111111KKOKOOOKO") "OK" else result
}
"#;

#[test]
fn values_derived_from_enhanced_java_results_are_checked_like_kotlinc() {
    let class = "EnhancedTypeArgumentKt";
    let pair = common::ModuleClassPair::compile(&[("EnhancedTypeArgument.kt", SOURCE)], class);
    for method in [
        "firstKey",
        "firstEntry",
        "firstValue",
        "nextKey",
        "nextElement",
        "element",
        "inferredKeys",
        "inferredEntries",
        "declaredEntries",
        "loopKey",
        "loopValue",
        "pickedMixed",
        "pickedMarked",
        "pickedNamed",
        "defaulted",
        "oneVararg",
        "manyVararg",
        "wholeVararg",
        "boxed",
        "paired",
        "boxedValue",
        "branchKey",
        "whenKey",
        "variableKey",
        "treeValue",
        "nestedElement",
        "text",
    ] {
        let (kotlinc, krusty) = pair.method_code(class, method);
        assert_eq!(krusty, kotlinc, "{class}.{method}");
    }
}

#[test]
fn values_derived_from_enhanced_java_results_run_like_kotlinc() {
    let jdk = common::jdk_modules();
    let output = common::compile_and_run_box(
        SOURCE,
        "EnhancedTypeArgument",
        &[common::stdlib_jar()],
        Some(jdk.as_path()),
    )
    .expect("krusty compiles and runs the box");
    assert_eq!(output, "OK");
}
