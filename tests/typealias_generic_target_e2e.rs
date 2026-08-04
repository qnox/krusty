//! A source `typealias` whose target is a CLASS type carrying TYPE ARGUMENTS
//! (`typealias IntList = List<Int>`, `typealias Table<V> = Map<String, V>`). The alias must expand
//! structurally — target name *and* type arguments — so members read through the alias keep their
//! substituted element types instead of collapsing to `Any`.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "TaGen")
}

#[test]
fn alias_of_generic_class_keeps_its_type_arguments() {
    // `xs[0]` through the alias must type as `Int`, not `Any`: `+ 1` is otherwise rejected with
    // "operator cannot be applied to 'Any' and 'Int'".
    const SRC: &str = "typealias IntList = List<Int>\n\
fun sum(xs: IntList): Int {\n\
    var acc = 0\n\
    for (x in xs) acc += x\n\
    return acc\n\
}\n\
fun head(xs: IntList): Int = xs[0] + 1\n\
fun box(): String {\n\
    val xs: IntList = listOf(1, 2, 3)\n\
    if (sum(xs) != 6) return \"f1\"\n\
    if (head(xs) != 2) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("alias of a generic class should keep its type arguments");
    assert_eq!(out, "OK");
}

#[test]
fn generic_alias_substitutes_into_a_class_target() {
    // `typealias Table<V> = Map<String, V>` — the use site's `<Int>` substitutes for `V`, so
    // `t[key]` types as `Int?` and the elvis branches agree.
    const SRC: &str = "typealias Table<V> = Map<String, V>\n\
fun lookup(t: Table<Int>, key: String): Int = t[key] ?: -1\n\
fun box(): String {\n\
    val t: Table<Int> = mapOf(\"a\" to 1)\n\
    if (lookup(t, \"a\") != 1) return \"f1\"\n\
    if (lookup(t, \"z\") != -1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("generic class-target alias should substitute its type arguments");
    assert_eq!(out, "OK");
}

#[test]
fn alias_of_a_nested_generic_target() {
    // The target's own type arguments may themselves be generic — the inner `List<Int>` must survive
    // the expansion, so the inner loop variable types as `Int`.
    const SRC: &str = "typealias Rows = List<List<Int>>\n\
fun total(rs: Rows): Int {\n\
    var acc = 0\n\
    for (r in rs) for (v in r) acc += v\n\
    return acc\n\
}\n\
fun box(): String {\n\
    if (total(listOf(listOf(1, 2), listOf(3))) != 6) return \"f1\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("alias of a nested generic target should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn nullable_use_site_keeps_its_question_mark() {
    // A `?` on the USE site survives the expansion onto the target type.
    const SRC: &str = "typealias Ints = List<Int>\n\
fun firstOr(xs: Ints?, fallback: Int): Int = if (xs == null) fallback else xs[0] + 1\n\
fun box(): String {\n\
    if (firstOr(listOf(4, 5), 9) != 5) return \"f1\"\n\
    if (firstOr(null, 9) != 9) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("a nullable alias use site should keep its '?'");
    assert_eq!(out, "OK");
}

#[test]
fn alias_chain_through_another_generic_alias() {
    // `typealias A<T> = List<T>` then `typealias B = A<Int>` — the re-expansion pass resolves the
    // chain before any member lookup runs.
    const SRC: &str = "typealias Seq<T> = List<T>\n\
typealias Ints = Seq<Int>\n\
fun first(xs: Ints): Int = xs[0] + 1\n\
fun box(): String {\n\
    if (first(listOf(4, 5)) != 5) return \"f1\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("a chained generic class alias should expand transitively");
    assert_eq!(out, "OK");
}
