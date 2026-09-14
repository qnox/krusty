//! A safe call to an inline collection transform keeps its checked iteration plan.
//!
//! `map`/`flatMap` are expanded structurally: resolution records the declaration-scoped iterator
//! protocol for the call, and checked FIR embeds it in the call's inline plan. Both sides key that
//! protocol by the RECEIVER expression — but resolution read the receiver out of `Expr::Call`'s
//! `Member` callee only, and a safe call is its own AST node. So `xs?.map { … }` filed the protocol
//! under the call expression while the checker looked for it under `xs`, the lookup missed, and a
//! transform with a lambda argument is a hard failure once its protocol is absent:
//! `internal error: checked FIR construction failed … MissingStableCallTarget`, which costs the
//! whole FILE.
//!
//! `?.forEach` was unaffected — its plan is published from the declaration body, not from this
//! protocol — which is what kept the gap narrow enough to miss.

use super::common;

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
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "safe_call_collection_transform");
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
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "safe_call_transform_suspension");
}

/// The spellings that already worked stay working, so the protocol key cannot start depending on
/// how the receiver is written.
#[test]
fn the_other_receiver_spellings_still_transform() {
    const MAIN: &str = "fun plain(xs: List<String>): List<Int> = xs.map { it.length }\n\
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
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "receiver_spellings_transform");
}
