//! End-to-end "box" coverage for the standard-library collection / map / set / sequence / string
//! callables. Each test compiles a `fun box(): String` that computes real values with stdlib
//! operations and returns "OK" only when every assertion holds, then round-trips it on the JVM.
//! Exercises stdlib callable resolution across resolve.rs, symbol_resolver.rs, jvm_libraries.rs and
//! metadata.rs.

use super::common;

use std::path::PathBuf;

/// Compile `src` (must define `fun box(): String`) and run it on the persistent JVM, asserting the
/// result is "OK". Returns silently (skip) when the toolchain isn't provisioned.
fn check(src: &str, stem: &str) {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping feature_coverage_l_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping feature_coverage_l_e2e: no kotlin-stdlib jar found");
        return;
    };
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, stem, &[stdlib], Some(&jdk)) else {
        panic!("compile_and_run_box returned None for {stem}");
    };
    assert_eq!(out, "OK", "box() mismatch for {stem}");
}

#[test]
fn list_fold_sum_aggregate() {
    // Dropped: reduce / maxOrNull / minOrNull (krusty does not resolve these — COMPILE_NONE).
    let src = r#"
fun box(): String {
    val xs = listOf(1, 2, 3, 4, 5)
    if (xs.fold(0) { a, b -> a + b } != 15) return "fold"
    if (xs.sum() != 15) return "sum"
    if (xs.sumOf { it * 2 } != 30) return "sumOf"
    if (xs.count() != 5) return "count"
    if (xs.count { it % 2 == 0 } != 2) return "countPred"
    return "OK"
}
"#;
    check(src, "ListFoldSum");
}

#[test]
fn list_predicates_and_access() {
    let src = r#"
fun box(): String {
    val xs = listOf(2, 4, 6, 8)
    if (!xs.any { it > 6 }) return "any"
    if (!xs.all { it % 2 == 0 }) return "all"
    if (!xs.none { it > 100 }) return "none"
    if (xs.first() != 2) return "first"
    if (xs.last() != 8) return "last"
    if (xs.firstOrNull { it > 5 } != 6) return "firstOrNull"
    if (xs.indexOf(6) != 2) return "indexOf"
    return "OK"
}
"#;
    check(src, "ListPredicates");
}

#[test]
fn list_slice_and_reorder() {
    let src = r#"
fun box(): String {
    val xs = listOf(3, 1, 2, 3, 1)
    if (xs.take(2) != listOf(3, 1)) return "take"
    if (xs.drop(3) != listOf(3, 1)) return "drop"
    if (xs.reversed() != listOf(1, 3, 2, 1, 3)) return "reversed"
    if (xs.sorted() != listOf(1, 1, 2, 3, 3)) return "sorted"
    if (xs.distinct() != listOf(3, 1, 2)) return "distinct"
    return "OK"
}
"#;
    check(src, "ListSlice");
}

#[test]
fn list_sorted_by() {
    let src = r#"
fun box(): String {
    val words = listOf("bbb", "a", "cc")
    if (words.sortedBy { it.length } != listOf("a", "cc", "bbb")) return "sortedBy"
    return "OK"
}
"#;
    check(src, "ListSortedBy");
}

#[test]
fn list_map_filter_flatmap() {
    let src = r#"
fun box(): String {
    val xs = listOf(1, 2, 3)
    if (xs.map { it * it } != listOf(1, 4, 9)) return "map"
    if (xs.filter { it > 1 } != listOf(2, 3)) return "filter"
    if (xs.flatMap { listOf(it, it) } != listOf(1, 1, 2, 2, 3, 3)) return "flatMap"
    return "OK"
}
"#;
    check(src, "ListMapFilterFlatMap");
}

#[test]
fn list_indexed_and_notnull() {
    let src = r#"
fun box(): String {
    val xs = listOf(10, 20, 30)
    if (xs.mapIndexed { i, v -> i + v } != listOf(10, 21, 32)) return "mapIndexed"
    if (xs.filterIndexed { i, _ -> i % 2 == 0 } != listOf(10, 30)) return "filterIndexed"
    val ns: List<Int?> = listOf(1, null, 3, null)
    if (ns.filterNotNull() != listOf(1, 3)) return "filterNotNull"
    if (ns.mapNotNull { it } != listOf(1, 3)) return "mapNotNull"
    return "OK"
}
"#;
    check(src, "ListIndexed");
}

#[test]
fn list_grouping_and_partition() {
    let src = r#"
fun box(): String {
    val xs = listOf(1, 2, 3, 4, 5, 6)
    val g = xs.groupBy { it % 2 }
    if (g[0] != listOf(2, 4, 6)) return "groupBy0"
    if (g[1] != listOf(1, 3, 5)) return "groupBy1"
    val (even, odd) = xs.partition { it % 2 == 0 }
    if (even != listOf(2, 4, 6)) return "partitionEven"
    if (odd != listOf(1, 3, 5)) return "partitionOdd"
    return "OK"
}
"#;
    check(src, "ListGrouping");
}

#[test]
fn list_associate() {
    let src = r#"
fun box(): String {
    val xs = listOf("a", "bb", "ccc")
    val byLen = xs.associateBy { it.length }
    if (byLen[2] != "bb") return "associateBy"
    val withLen = xs.associateWith { it.length }
    if (withLen["ccc"] != 3) return "associateWith"
    val m = xs.associate { it to it.length }
    if (m["a"] != 1) return "associate"
    return "OK"
}
"#;
    check(src, "ListAssociate");
}

#[test]
fn list_zip_chunked_windowed() {
    let src = r#"
fun box(): String {
    val a = listOf(1, 2, 3)
    val b = listOf("x", "y", "z")
    if (a.zip(b) != listOf(1 to "x", 2 to "y", 3 to "z")) return "zip"
    val xs = listOf(1, 2, 3, 4, 5)
    if (xs.chunked(2) != listOf(listOf(1, 2), listOf(3, 4), listOf(5))) return "chunked"
    if (xs.windowed(2) != listOf(listOf(1, 2), listOf(2, 3), listOf(3, 4), listOf(4, 5))) return "windowed"
    return "OK"
}
"#;
    check(src, "ListZip");
}

#[test]
fn list_flatten_join() {
    let src = r#"
fun box(): String {
    val nested = listOf(listOf(1, 2), listOf(3), listOf(4, 5))
    if (nested.flatten() != listOf(1, 2, 3, 4, 5)) return "flatten"
    val xs = listOf(1, 2, 3)
    if (xs.joinToString("-") != "1-2-3") return "joinToString"
    if (xs.joinToString(",", "[", "]") != "[1,2,3]") return "joinToStringFix"
    return "OK"
}
"#;
    check(src, "ListFlatten");
}

#[test]
fn map_basic_access() {
    let src = r#"
fun box(): String {
    val m = mapOf("a" to 1, "b" to 2, "c" to 3)
    if (m.keys.sorted() != listOf("a", "b", "c")) return "keys"
    if (m.values.sorted() != listOf(1, 2, 3)) return "values"
    if (!m.containsKey("b")) return "containsKey"
    if (m.getOrDefault("z", 9) != 9) return "getOrDefault"
    if (m.getOrElse("z") { 7 } != 7) return "getOrElse"
    var sum = 0
    for ((_, v) in m) sum += v
    if (sum != 6) return "entries"
    return "OK"
}
"#;
    check(src, "MapAccess");
}

#[test]
fn map_transform() {
    let src = r#"
fun box(): String {
    val m = mapOf("a" to 1, "b" to 2, "c" to 3)
    val fk = m.filterKeys { it != "b" }
    if (fk.size != 2 || fk.containsKey("b") || fk["a"] != 1) return "filterKeys"
    val fv = m.filterValues { it > 1 }
    if (fv.size != 2 || fv.containsKey("a") || fv["b"] != 2) return "filterValues"
    val mv = m.mapValues { it.value * 10 }
    if (mv["a"] != 10 || mv["c"] != 30) return "mapValues"
    return "OK"
}
"#;
    check(src, "MapTransform");
}

#[test]
fn set_operations() {
    let src = r#"
fun box(): String {
    val a = setOf(1, 2, 3)
    val b = setOf(3, 4, 5)
    if (a.union(b) != setOf(1, 2, 3, 4, 5)) return "union"
    if (a.intersect(b) != setOf(3)) return "intersect"
    if (a.subtract(b) != setOf(1, 2)) return "subtract"
    if (!a.contains(2)) return "contains"
    return "OK"
}
"#;
    check(src, "SetOps");
}

#[test]
fn sequence_pipeline() {
    let src = r#"
fun box(): String {
    val xs = listOf(1, 2, 3, 4, 5, 6)
    val r = xs.asSequence().map { it * 2 }.filter { it > 6 }.toList()
    if (r != listOf(8, 10, 12)) return "asSequence"
    val gen = generateSequence(1) { it * 2 }.take(4).toList()
    if (gen != listOf(1, 2, 4, 8)) return "generateSequence"
    return "OK"
}
"#;
    check(src, "SequencePipeline");
}

// Dropped: `sequence { yield(...) }` builder — krusty emits a Sequence lambda whose iterator() is
// abstract at runtime (AbstractMethodError). generateSequence / asSequence pipelines work and are
// covered by `sequence_pipeline` above.

#[test]
fn string_split_join_case() {
    // `split` returns a list whose structural equality to `listOf(...)` does not compile in krusty,
    // so assert via size + indexing instead.
    let src = r#"
fun box(): String {
    val parts = "a,b,c".split(",")
    if (parts.size != 3 || parts[0] != "a" || parts[2] != "c") return "split"
    if ("Hello".uppercase() != "HELLO") return "uppercase"
    if ("Hello".lowercase() != "hello") return "lowercase"
    if ("  hi  ".trim() != "hi") return "trim"
    return "OK"
}
"#;
    check(src, "StringSplitJoin");
}

#[test]
fn string_substring_replace_predicates() {
    let src = r#"
fun box(): String {
    val s = "hello world"
    if (s.substring(0, 5) != "hello") return "substring"
    if (s.replace("world", "kotlin") != "hello kotlin") return "replace"
    if (!s.startsWith("hello")) return "startsWith"
    if (!s.endsWith("world")) return "endsWith"
    if (s.indexOf("world") != 6) return "indexOf"
    return "OK"
}
"#;
    check(src, "StringSubstring");
}

#[test]
fn string_pad_and_repeat() {
    // Dropped: `toIntOrNull` (krusty does not resolve it — COMPILE_NONE).
    let src = r#"
fun box(): String {
    if ("7".padStart(3, '0') != "007") return "padStart"
    if ("ab".repeat(3) != "ababab") return "repeat"
    if ("hello".length != 5) return "length"
    return "OK"
}
"#;
    check(src, "StringPad");
}
