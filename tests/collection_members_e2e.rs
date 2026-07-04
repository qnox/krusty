//! Member + extension resolution on Kotlin MAPPED collection types (`kotlin.collections.List`/`Set`/…),
//! which have no own `.class` — their *actual* platform type is a JVM interface (`java/util/List`), the
//! `expect`/`actual` + `JavaToKotlinClassMap` device kotlinc uses. `resolve_type` now falls back to that
//! mapped type (generic `to_jvm_internal`), so `for (x in list)`, `list[i]`, `list.size`,
//! `list.iterator()`, and stdlib extensions (`forEach`/`contains`/`indexOf`) resolve. The iterator
//! protocol is byte-identical to kotlinc (`java/util/List.iterator()` / `Iterator.hasNext()`/`next()`).
//! Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn for_over_list() {
    const SRC: &str =
        "fun box(): String { var s = \"\"; for (x in listOf(\"O\", \"K\")) s += x; return s }\n";
    assert_eq!(run(SRC).expect("for-over-List compiles + runs"), "OK");
}

#[test]
fn list_size_and_index() {
    const SRC: &str = "fun box(): String { val l = listOf(\"O\", \"K\"); return if (l.size == 2) l[0] + l[1] else \"no\" }\n";
    assert_eq!(run(SRC).expect("list size+index compiles + runs"), "OK");
}

#[test]
fn list_extension_members() {
    const SRC: &str = "fun box(): String {\n\
    val l = listOf(\"O\", \"K\")\n\
    return if (!l.isEmpty() && l.contains(\"O\") && l.indexOf(\"K\") == 1) \"OK\" else \"no\"\n\
}\n";
    assert_eq!(run(SRC).expect("list members compile + run"), "OK");
}

#[test]
fn build_map_put_statement_discards_nullable_previous_value() {
    const SRC: &str = "// WITH_STDLIB\n\
fun box(): String {\n\
    val map = buildMap {\n\
        put(1, 1)\n\
        for (v in values) {}\n\
    }\n\
    return if (map[1] == 1) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("buildMap put statement compiles + runs"),
        "OK"
    );
}

#[test]
fn numeric_reduction_extensions_by_element_type() {
    // `sum()`/`average()` are `@JvmName`-mangled by the receiver's ELEMENT type (`List<Int>.sum()` →
    // the bytecode method `sumOfInt`, `List<Long>.sum()` → `sumOfLong`, `average()` → `averageOfInt`).
    // krusty derives the mangled name from the element and binds the element-appropriate overload.
    const SRC: &str = "fun box(): String {\n\
    val a = listOf(1, 2, 3).sum()\n\
    val b = listOf(1L, 2L).sum()\n\
    val c = listOf(1.5, 2.5).average()\n\
    return if (a == 6 && b == 3L && c == 2.0) \"OK\" else \"FAIL: $a/$b/$c\"\n\
}\n";
    assert_eq!(run(SRC).expect("numeric reductions compile + run"), "OK");
}

#[test]
fn numeric_reductions_cover_element_types_and_average() {
    // Broader element/return coverage: `Double`/`Long` sums (distinct return types), `average()` over
    // an `Int` list (returns `Double`), and a `sum()` that flows straight into arithmetic. Each binds a
    // DIFFERENT `@JvmName` overload by element (`sumOfDouble`/`sumOfLong`/`averageOfInt`).
    const SRC: &str = "fun box(): String {\n\
    val d: Double = listOf(1.5, 2.5, 3.0).sum()\n\
    val l: Long = listOf(10L, 20L, 30L).sum()\n\
    val avg: Double = listOf(2, 4, 6).average()\n\
    val chained: Int = listOf(1, 2, 3, 4).sum() * 2\n\
    return if (d == 7.0 && l == 60L && avg == 4.0 && chained == 20) \"OK\" else \"FAIL: $d/$l/$avg/$chained\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("reduction element/return coverage compiles + runs"),
        "OK"
    );
}
