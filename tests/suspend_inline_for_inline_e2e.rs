//! A suspend inline function keeps a `$$forInline` copy of its body for inlining callers, beside
//! the state machine a direct caller (reflection, a Java caller) runs. The copy is private, keeps the
//! suspension markers around each suspend call, and no optimizer pass runs over it. A function only
//! an inlining caller can call (reified, or private) has no state machine: its one method holds the
//! marked body.

use super::common;

const LIB: &str = "class Stmt(val v: String) {\n\
    \x20   suspend fun exec(): Any = v\n\
    \x20   suspend inline fun twice(): String = exec().toString() + exec()\n\
    }\n\
    suspend fun step(): Int = 1\n\
    suspend inline fun plain(x: Int): Int = x + step()\n\
    suspend inline fun around(block: () -> Int): Int = step() + block()\n\
    suspend inline fun suspending(block: suspend () -> Int): Int = block() + step()\n\
    suspend inline fun <reified T> Stmt.body(): T = exec() as T\n";

#[test]
fn a_suspend_inline_function_keeps_the_reference_compilers_copy_for_inlining() {
    common::assert_classes_identical_to_kotlinc("SuspendInline", LIB, &["Stmt", "SuspendInlineKt"]);
}

const HIDDEN: &str = "private suspend inline fun hidden(): Int = step() + step()\n\
    suspend fun useHidden(): Int = hidden()\n";

const MAIN: &str = "import kotlin.coroutines.*\n\
    fun box(): String {\n\
    \x20   var r = \"\"\n\
    \x20   suspend {\n\
    \x20       val s = Stmt(\"a\")\n\
    \x20       r = s.twice() + plain(1) + around { 2 } + suspending { step() + 3 } + s.body<String>() + useHidden()\n\
    \x20   }.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
    \x20   return if (r == \"aa235a2\") \"OK\" else r\n\
    }\n";

#[test]
fn suspend_inline_functions_of_a_dependency_inline_and_run() {
    let lib = format!("{LIB}{HIDDEN}");
    if let Some(result) = common::expect_box_run_against("suspinl", &lib, MAIN) {
        assert_eq!(result, "OK");
    }
}
