//! Suspend functions and coroutines through krusty's own code generator.
//!
//! A suspend function becomes a frame class and a state machine in common IR (`backend::coroutines`
//! and `native::coroutines`); the runtime keeps only the library's entry points. Each program here
//! runs on the JVM backend too, so its answer is Kotlin's rather than this target's opinion of it.
//! Every one SUSPENDS for real: a continuation is stored, the caller goes on, and something resumes
//! it later, which is the path a body that only ever returns directly never takes.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

fn expect_ok_on_both(source: &str, stem: &str) {
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// A driver that runs every postponed resumption to completion, as an event loop would.
const DRIVER: &str = r#"
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

val pending = ArrayList<() -> Unit>()

suspend fun <T> later(value: T): T = suspendCoroutine { c -> pending.add { c.resume(value) } }

suspend fun fail(message: String): Nothing =
    suspendCoroutine { c -> pending.add { c.resumeWithException(IllegalStateException(message)) } }

fun <T> run(block: suspend () -> T): Result<T> {
    var outcome: Result<T>? = null
    block.startCoroutine(Continuation(EmptyCoroutineContext) { outcome = it })
    while (pending.size > 0) pending.removeAt(0)()
    return outcome!!
}
"#;

fn program(body: &str) -> String {
    format!("{DRIVER}\n{body}")
}

#[test]
fn a_suspended_call_resumes_with_its_value() {
    let source = program(
        "suspend fun twice(s: String): String = later(s) + later(s)\n\
         fun box(): String = run { twice(\"O\") + later(\"K\") }.getOrThrow().let { if (it == \"OOK\") \"OK\" else it }\n",
    );
    expect_ok_on_both(&source, "SuspendedCallResumes");
}

#[test]
fn locals_and_loops_survive_a_suspension() {
    let source = program(
        "suspend fun sum(n: Int): Int {\n\
         \x20   var total = 0\n\
         \x20   for (i in 1..n) total += later(i)\n\
         \x20   var j = 0\n\
         \x20   while (later(j) < 3) j++\n\
         \x20   do { total += 100 } while (later(false))\n\
         \x20   return total + j\n\
         }\n\
         fun box(): String = run { sum(4) }.getOrThrow().let { if (it == 113) \"OK\" else \"$it\" }\n",
    );
    expect_ok_on_both(&source, "LocalsAndLoopsSurvive");
}

#[test]
fn an_exception_resumed_into_a_frame_reaches_its_catch_and_finally() {
    let source = program(
        "var log = \"\"\n\
         suspend fun guarded(): String {\n\
         \x20   try {\n\
         \x20       log += \"t\"\n\
         \x20       fail(\"x\")\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       log += e.message + later(\"c\")\n\
         \x20   } finally {\n\
         \x20       log += later(\"f\")\n\
         \x20   }\n\
         \x20   try {\n\
         \x20       return later(\"r\")\n\
         \x20   } finally {\n\
         \x20       log += later(\"g\")\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   val answer = run { guarded() }.getOrThrow()\n\
         \x20   return if (answer == \"r\" && log == \"txcfg\") \"OK\" else \"$answer $log\"\n\
         }\n",
    );
    expect_ok_on_both(&source, "ExceptionResumedIntoFrame");
}

#[test]
fn an_uncaught_exception_completes_the_coroutine_with_a_failure() {
    let source = program(
        "suspend fun boom(): Int { later(1); fail(\"boom\") }\n\
         fun box(): String {\n\
         \x20   val outcome = run { boom() }\n\
         \x20   return if (outcome.isFailure && outcome.exceptionOrNull()?.message == \"boom\") \"OK\" else \"$outcome\"\n\
         }\n",
    );
    expect_ok_on_both(&source, "UncaughtCompletesWithFailure");
}

#[test]
fn a_value_class_and_a_result_cross_a_suspension_boxed_as_a_type_argument() {
    let source = program(
        "@JvmInline value class Name(val text: String)\n\
         suspend fun name(): Name = later(Name(\"O\"))\n\
         suspend fun result(): Result<String> = later(Result.success(\"K\"))\n\
         fun box(): String = run { name().text + result().getOrThrow() }.getOrThrow()\n",
    );
    expect_ok_on_both(&source, "ValueClassAcrossSuspension");
}

#[test]
fn a_continuation_resumed_before_suspend_coroutine_returns_goes_on_without_suspending() {
    let source = program(
        "suspend fun now(): String = suspendCoroutine { it.resume(\"O\") }\n\
         suspend fun raw(): String = suspendCoroutineUninterceptedOrReturn { \"K\" }\n\
         fun box(): String = run { now() + raw() }.getOrThrow()\n",
    );
    expect_ok_on_both(&source, "ResumedBeforeReturn");
}

#[test]
fn a_created_coroutine_starts_when_first_resumed() {
    let source = program(
        "fun box(): String {\n\
         \x20   var out = \"unstarted\"\n\
         \x20   val start = suspend { later(\"OK\") }.createCoroutine(Continuation(EmptyCoroutineContext) { out = it.getOrThrow() })\n\
         \x20   if (out != \"unstarted\") return out\n\
         \x20   start.resume(Unit)\n\
         \x20   while (pending.size > 0) pending.removeAt(0)()\n\
         \x20   return out\n\
         }\n",
    );
    expect_ok_on_both(&source, "CreatedCoroutineStarts");
}

#[test]
fn suspend_lambdas_references_and_context_suspend_through_their_invoke() {
    let source = program(
        "suspend fun twice(s: String): String = later(s) + s\n\
         suspend fun context(): CoroutineContext = coroutineContext\n\
         fun box(): String {\n\
         \x20   val reference: suspend (String) -> String = ::twice\n\
         \x20   val lambda: suspend (Unit) -> String = { later(\"K\") }\n\
         \x20   val answer = run { reference(\"O\").length.toString() + lambda(Unit) + (context() === EmptyCoroutineContext) }\n\
         \x20   return if (answer.getOrThrow() == \"2Ktrue\") \"OK\" else answer.toString()\n\
         }\n",
    );
    expect_ok_on_both(&source, "LambdasReferencesContext");
}

#[test]
fn a_primitive_result_crosses_a_suspend_call_unboxed() {
    let source = program(
        "suspend fun now(): Int = 42\n\
         suspend fun resumed(): Int = later(40) + 2\n\
         fun box(): String {\n\
         \x20   var direct = -1\n\
         \x20   val answer = run { direct = now(); resumed() }.getOrThrow()\n\
         \x20   return if (direct == 42 && answer == 42) \"OK\" else \"F:$direct:$answer\"\n\
         }\n",
    );
    expect_ok_on_both(&source, "PrimitiveResultUnboxed");
}
