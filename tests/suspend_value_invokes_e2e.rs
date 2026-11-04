//! A function value the call passes for an inline suspend parameter without inlining it is
//! invoked, in the regenerated copy of an object the inline function creates, as a suspension
//! point of the copy's machine: kotlinc's `surroundInvokesWithSuspendMarkersIfNeeded`. The
//! library's `$$forInline` body leaves the invoke unmarked, since its compiler cannot know whether
//! a call inlines the lambda; the inliner marks it once it knows the call does not.

use super::common;

const LIB: &str = r#"
package lib

fun <T, R> deferred(block: suspend (T) -> R): suspend (T) -> R = block

inline fun <T, R> chained(
    crossinline first: suspend (T) -> R,
    crossinline then: suspend (R) -> R,
): suspend (T) -> R = deferred { value -> then(first(value)) }

inline fun <T> held(
    crossinline first: suspend (T) -> T,
    crossinline then: suspend (T) -> T,
): suspend (T) -> T = deferred { value -> listOf(then(first(value))).first() }

interface Step {
    suspend fun run(value: Int): Int
}

inline fun stepped(
    crossinline first: suspend (Int) -> Int,
    crossinline then: suspend (Int) -> Int,
): Step = object : Step {
    override suspend fun run(value: Int): Int = then(first(value)) * 2
}
"#;

const CALLS: &str = r#"
fun tail(step: suspend (Int) -> Int): suspend (Int) -> Int = chained({ it + 1 }, step)

fun kept(step: suspend (Int) -> Int): suspend (Int) -> Int = held({ it + 1 }, step)

fun named(step: suspend (Int) -> Int): Step = stepped({ it + 1 }, step)
"#;

/// A suspend lambda's machine suspends at the invoke, in tail position and with a value still
/// to be used after it, and so does a named suspend method's: each copy is kotlinc's, byte for
/// byte.
#[test]
fn an_uninlined_suspend_value_is_a_suspension_point_of_the_copy_like_kotlinc() {
    let main = format!("import lib.*\n{CALLS}");
    let classes = common::classes_against_kotlinc_lib("Values", &[("Lib.kt", LIB)], &main)
        .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "ValuesKt",
            "ValuesKt$kept$$inlined$held$1",
            "ValuesKt$named$$inlined$stepped$1",
            "ValuesKt$named$$inlined$stepped$1$1",
            "ValuesKt$tail$$inlined$chained$1",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// Each copy suspends inside the function value and resumes through its own machine with the
/// value's result.
#[test]
fn an_uninlined_suspend_value_suspends_and_resumes_the_copy() {
    let main = format!(
        r#"import lib.*
import kotlin.coroutines.*
{CALLS}
var pending: Continuation<Int>? = null

suspend fun later(): Int = suspendCoroutine {{ pending = it }}

fun drive(block: suspend () -> Int, resumed: Int): Any? {{
    var result: Any? = null
    block.startCoroutine(Continuation(EmptyCoroutineContext) {{ result = it.getOrThrow() }})
    if (result != null) return "completed before resuming: $result"
    pending!!.resume(resumed)
    return result
}}

fun box(): String {{
    val tailed = drive({{ tail {{ it + later() }}(1) }}, 100)
    if (tailed != 102) return "FAIL tail: $tailed"
    val kept = drive({{ kept {{ it + later() }}(1) }}, 10)
    if (kept != 12) return "FAIL kept: $kept"
    val named = drive({{ named {{ it + later() }}.run(1) }}, 10)
    if (named != 24) return "FAIL named: $named"
    return "OK"
}}
"#
    );
    let output = common::expect_box_run_against_ref("suspend_value_invokes", LIB, &main)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}
