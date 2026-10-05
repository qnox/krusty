//! `suspendCoroutine` is inlined from the stdlib like any other `@InlineOnly` suspend function, as
//! kotlinc does: its body's suspension markers and `SafeContinuation` protocol land in the caller's
//! state machine, the inlined body reads the caller's continuation local, and the locals live
//! across the inline call are spilled around its suspension. The transformed method's code and
//! debug lines match kotlinc's, and the coroutine suspends and resumes the same way.

use super::common;

const SOURCE: &str = r#"import kotlin.coroutines.*

var pending: Any? = null
var trace = ""

fun join(head: Any?, value: Any?, tail: Any?): String = "$head$value$tail"

suspend fun head(): String = suspendCoroutine { continuation ->
    trace += "head;"
    continuation.resume("O")
}

suspend fun stored(tail: String): String {
    val first = head()
    val second = suspendCoroutine<Any?> { continuation ->
        trace += "suspended;"
        pending = continuation
    }
    return join(first, second, tail)
}

class Completion : Continuation<String> {
    override val context: CoroutineContext
        get() = EmptyCoroutineContext

    override fun resumeWith(result: Result<String>) {
        trace += result.getOrThrow()
    }
}

fun box(): String {
    val body: suspend () -> String = { stored("") }
    body.startCoroutine(Completion())
    if (trace != "head;suspended;") return trace
    @Suppress("UNCHECKED_CAST")
    (pending as Continuation<Any?>).resume("K")
    return if (trace == "head;suspended;OK") "OK" else trace
}
"#;

#[test]
fn suspend_coroutine_is_inlined_into_the_callers_state_machine_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendCoroutineInline",
        SOURCE,
        &[
            "SuspendCoroutineInlineKt",
            "SuspendCoroutineInlineKt$stored$1",
        ],
    );
}

#[test]
fn suspend_coroutine_suspends_and_resumes_like_kotlinc() {
    common::expect_box_same_as_kotlinc(SOURCE, "SuspendCoroutineInline");
}

/// The resumed result narrowed to the local's type. kotlinc's `visitVariable` marks the
/// initializer's line before it materializes the inline call's erased result, and the inlined body
/// left the caller's line forgotten, so the `checkcast` sits on the call's line (behind the `nop`
/// the resumption keeps for the transformer's own entry on that line).
const TYPED_SOURCE: &str = r#"import kotlin.coroutines.*

var pending: Any? = null

fun take(value: Any?): String = "K"

suspend fun typed(tail: String): String {
    val second = suspendCoroutine<String> { continuation ->
        pending = continuation
    }
    return take(second) + tail
}
"#;

#[test]
fn a_resumed_inline_result_is_narrowed_on_the_calls_line_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendCoroutineTyped",
        TYPED_SOURCE,
        &["SuspendCoroutineTypedKt", "SuspendCoroutineTypedKt$typed$1"],
    );
}

/// A repository-owned inline suspend function on the classpath, whose erased `T` result the caller
/// narrows after resuming. The inlined body leaves the caller's line forgotten whatever the callee,
/// so the cast is on the call's line here too.
const PARKING: &str = r#"package parking

import kotlin.coroutines.*

var parked: Any? = null

suspend inline fun <T> park(): T = suspendCoroutine { continuation -> parked = continuation }
"#;

const PARKED_SOURCE: &str = r#"import parking.*

fun take(value: Any?): String = "K"

suspend fun parkedResult(tail: String): String {
    val second = park<String>()
    return take(second) + tail
}
"#;

#[test]
fn a_classpath_inline_suspend_result_is_narrowed_on_the_calls_line_like_kotlinc() {
    let library =
        common::kotlinc_library(PARKING).expect("reference compiler builds the dependency");
    common::assert_classes_identical_to_kotlinc_against(
        "SuspendInlineParked",
        PARKED_SOURCE,
        &["SuspendInlineParkedKt", "SuspendInlineParkedKt$parkedResult$1"],
        &[library],
    );
}
