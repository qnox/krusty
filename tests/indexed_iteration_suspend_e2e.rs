//! An inline plan's dependency stays resolvable when the classpath reaches it twice.
//!
//! The indexed-iteration recognizer reads `forEachIndexed`'s `checkIndexOverflow` guard and resolves
//! the overflow target it calls — `kotlin.collections.throwIndexOverflow`. That resolution demanded
//! exactly ONE matching candidate. A classpath that reaches the declaring jar through two entries
//! answers with the same method twice: same owner, same name, same descriptor — one JVM method by
//! definition. The "more than one" test read that as an ambiguity, rejected the dependency, and
//! `forEachIndexed` ended up with no inline plan at all.
//!
//! Without a plan the lambda is never expanded at IR level, so a suspension inside it never receives
//! the caller's continuation and the backend bails the whole FILE with `call arity mismatch`. A real
//! build classpath reaches one jar through several entries routinely, which is why this only ever
//! showed up outside the test suite.
//!
//! `forEach` was unaffected: a plain iteration plan resolves no overflow dependency.

use super::common;

/// Two classpath entries reaching the same jar — the shape a real build produces.
fn duplicated_stdlib() -> Vec<std::path::PathBuf> {
    let stdlib = common::stdlib_jar();
    vec![stdlib.clone(), stdlib]
}

/// The failing shape: a suspending call inside `forEachIndexed`, compiled by the real driver.
#[test]
fn the_driver_accepts_a_suspension_inside_for_each_indexed() {
    const MAIN: &str = "import kotlin.coroutines.*\n\
suspend fun twice(value: Int): Int = suspendCoroutine { it.resume(value * 2) }\n\
suspend fun weigh(xs: List<Int>): Int {\n\
\x20   var total = 0\n\
\x20   xs.forEachIndexed { index, value -> total += index * twice(value) }\n\
\x20   return total\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &duplicated_stdlib());
    assert_eq!(
        result.reference_code, 0,
        "kotlinc rejected the fixture: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 0,
        "krusty rejected a kotlinc-valid fixture: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}

/// The expansion must be the right loop, not merely present: zero-based positions, and a
/// `return@forEachIndexed` acting as `continue`. Runs the suspending body to completion.
#[test]
fn the_indexed_loop_keeps_its_positions_and_continue() {
    const MAIN: &str = "import kotlin.coroutines.*\n\
\n\
suspend fun identity(value: Int): Int = suspendCoroutine { it.resume(value) }\n\
\n\
suspend fun collect(xs: List<Int?>): String {\n\
\x20   val seen = StringBuilder()\n\
\x20   xs.forEachIndexed { index, value ->\n\
\x20       val present = value ?: return@forEachIndexed\n\
\x20       seen.append(index).append(':').append(identity(present)).append(' ')\n\
\x20   }\n\
\x20   return seen.toString().trim()\n\
}\n\
fun box(): String {\n\
\x20   var result = \"FAIL: never resumed\"\n\
\x20   val task: suspend () -> String = { collect(listOf(7, null, 9)) }\n\
\x20   task.startCoroutine(Continuation(EmptyCoroutineContext) {\n\
\x20       result = if (it.getOrThrow() == \"0:7 2:9\") \"OK\" else \"FAIL: \" + it.getOrThrow()\n\
\x20   })\n\
\x20   return result\n\
}\n";
    let out = common::compile_and_run_box_files(
        &[("Main.kt", MAIN)],
        &duplicated_stdlib(),
        Some(common::jdk_modules().as_path()),
    )
    .expect("indexed iteration with a suspension must compile and run");
    assert_eq!(out, "OK");
}

/// The non-indexed sibling and the ordinary non-suspending spelling keep working on the same
/// duplicated classpath.
#[test]
fn plain_iteration_and_non_suspending_indexing_still_work() {
    const MAIN: &str = "import kotlin.coroutines.*\n\
\n\
suspend fun twice(value: Int): Int = suspendCoroutine { it.resume(value * 2) }\n\
\n\
suspend fun plain(xs: List<Int>): Int {\n\
\x20   var total = 0\n\
\x20   xs.forEach { total += twice(it) }\n\
\x20   return total\n\
}\n\
fun direct(xs: List<Int>): Int {\n\
\x20   var total = 0\n\
\x20   xs.forEachIndexed { index, value -> total += index * value }\n\
\x20   return total\n\
}\n\
fun box(): String {\n\
\x20   if (direct(listOf(1, 2, 3)) != 8) return \"FAIL: direct\"\n\
\x20   var result = \"FAIL: never resumed\"\n\
\x20   val task: suspend () -> Int = { plain(listOf(1, 2)) }\n\
\x20   task.startCoroutine(Continuation(EmptyCoroutineContext) {\n\
\x20       result = if (it.getOrThrow() == 6) \"OK\" else \"FAIL: \" + it.getOrThrow()\n\
\x20   })\n\
\x20   return result\n\
}\n";
    let out = common::compile_and_run_box_files(
        &[("Main.kt", MAIN)],
        &duplicated_stdlib(),
        Some(common::jdk_modules().as_path()),
    )
    .expect("plain iteration must compile and run");
    assert_eq!(out, "OK");
}
