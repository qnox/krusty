//! A safe call to an inline collection transform keeps its checked iteration plan.
//!
//! `map`/`flatMap` are expanded structurally. Their provider plan carries the exact traversal,
//! factory, and append declaration identities read from the selected stdlib declaration; a safe
//! call changes only source evaluation and never creates a second semantic lookup or AST-keyed
//! protocol side table.

use super::common;

fn assert_clean_compile_and_runtime(source: &str, tag: &str) {
    let diagnostics = common::compiler_diagnostics(
        &[("Main.kt", source)],
        std::slice::from_ref(&common::stdlib_jar()),
    );
    assert_eq!(
        (
            diagnostics.reference_code,
            diagnostics.reference_stderr.as_str(),
            diagnostics.krusty_code,
            diagnostics.krusty_stdout.as_str(),
            diagnostics.krusty_stderr.as_str(),
        ),
        (0, "", 0, "", ""),
        "both compilers must accept the exact fixture without diagnostics"
    );
    let reference = common::kotlinc_box_result(source);
    assert_eq!(reference, "OK", "kotlinc runtime result for {tag}");
    assert_eq!(
        common::expect_box_run_with_stdlib(source, tag),
        reference,
        "krusty and kotlinc runtime results for {tag}"
    );
}

/// The failing shape, on both transforms that use the structural plan.
#[test]
fn a_safe_call_to_a_collection_transform_compiles() {
    const MAIN: &str = "fun mapped(xs: List<String>?): List<Int>? = xs?.map { it.length }\n\
fun flattened(xs: List<List<String>>?): List<String>? = xs?.flatMap { it }\n\
fun box(): String {\n\
\x20   if (mapped(listOf(\"ab\", \"c\")) != listOf(2, 1)) return \"FAIL: mapped\"\n\
\x20   if (mapped(null) != null) return \"FAIL: mapped null\"\n\
\x20   if (flattened(listOf(listOf(\"a\"), listOf(\"b\"))) != listOf(\"a\", \"b\")) return \"FAIL: flattened\"\n\
\x20   if (flattened(null) != null) return \"FAIL: flattened null\"\n\
\x20   return \"OK\"\n\
}\n";
    assert_clean_compile_and_runtime(MAIN, "safe_call_collection_transform");
}

/// The plan is not decoration: a suspension inside the lambda joins the CALLER's state machine, and
/// that only works when the transform is expanded structurally. A safe call must reach the same
/// expansion as the plain spelling, so assert the suspending body actually runs to completion.
#[test]
fn a_suspension_inside_a_safe_call_transform_runs() {
    const MAIN: &str = "import kotlin.coroutines.*\n\
\n\
suspend fun twice(value: Int): Int = suspendCoroutine { it.resume(value * 2) }\n\
\n\
suspend fun doubled(xs: List<Int>?): List<Int>? = xs?.map { twice(it) }\n\
\n\
fun box(): String {\n\
\x20   var result = \"FAIL: never resumed\"\n\
\x20   val task: suspend () -> List<Int>? = { doubled(listOf(1, 2)) }\n\
\x20   task.startCoroutine(Continuation(EmptyCoroutineContext) {\n\
\x20       result = if (it.getOrThrow() == listOf(2, 4)) \"OK\" else \"FAIL: \" + it.getOrThrow()\n\
\x20   })\n\
\x20   return result\n\
}\n";
    assert_clean_compile_and_runtime(MAIN, "safe_call_transform_suspension");
}

/// A map receiver's declaration body has a two-step `entries` then `iterator` traversal. This
/// reaches common lowering with that complete provider-owned chain while a suspension forces the
/// structural expansion to participate in the caller's state machine.
#[test]
fn a_suspending_safe_map_receiver_threads_every_traversal_identity() {
    const MAIN: &str = "import kotlin.coroutines.*\n\
\n\
suspend fun key(entry: Map.Entry<String, Int>): String =\n\
    suspendCoroutine { it.resume(entry.key + entry.value) }\n\
\n\
suspend fun rendered(values: Map<String, Int>?): List<String>? =\n\
    values?.map { key(it) }\n\
\n\
fun box(): String {\n\
\x20   var result = \"FAIL: never resumed\"\n\
\x20   val task: suspend () -> List<String>? = { rendered(mapOf(\"a\" to 1, \"b\" to 2)) }\n\
\x20   task.startCoroutine(Continuation(EmptyCoroutineContext) {\n\
\x20       result = if (it.getOrThrow() == listOf(\"a1\", \"b2\")) \"OK\" else \"FAIL: \" + it.getOrThrow()\n\
\x20   })\n\
\x20   return result\n\
}\n";
    assert_clean_compile_and_runtime(MAIN, "safe_map_receiver_suspension");
}

/// The spellings that already worked stay working, and a caller-scope same-spelled iterator cannot
/// replace the provider-owned traversal.
#[test]
fn receiver_spellings_and_iterator_shadow_keep_the_declaration_plan() {
    const MAIN: &str =
        "operator fun <T> List<T>.iterator(): Iterator<T> = emptyList<T>().iterator()\n\
fun plain(xs: List<String>): List<Int> = xs.map { it.length }\n\
fun elvis(xs: List<String>?): List<Int> = (xs ?: emptyList()).map { it.length }\n\
fun each(xs: List<String>?): Int {\n\
\x20   var total = 0\n\
\x20   xs?.forEach { total += it.length }\n\
\x20   return total\n\
}\n\
fun box(): String {\n\
\x20   if (plain(listOf(\"ab\")) != listOf(2)) return \"FAIL: plain\"\n\
\x20   if (elvis(null) != emptyList<Int>()) return \"FAIL: elvis\"\n\
\x20   if (each(listOf(\"ab\", \"c\")) != 3) return \"FAIL: each\"\n\
\x20   return \"OK\"\n\
}\n";
    assert_clean_compile_and_runtime(MAIN, "receiver_spellings_transform");
}

#[test]
fn a_user_defined_same_named_call_remains_an_ordinary_call() {
    const MAIN: &str = "class Bag<T>(val value: T)\n\
fun <T, R> Bag<T>.map(transform: (T) -> R): R = transform(value)\n\
fun mapped(bag: Bag<String>?): String? = bag?.map { it + \"K\" }\n\
fun box(): String = mapped(Bag(\"O\")) ?: \"FAIL\"\n";
    assert_clean_compile_and_runtime(MAIN, "user_defined_safe_map");
}
