//! A same-module inline call written in a suspend function or a suspend lambda is inlined from the
//! callee's compiled body, as kotlinc compiles it: the method inliner places the call's lambdas in
//! the caller's frame, and the coroutine transformer then builds the caller's machine over the
//! inlined bytecode, so a suspension in a placed lambda is a state of the caller.

use super::common;

const DECLARATION: &str = r#"inline fun <R> applied(value: R, block: (R) -> R): R = block(value ?: return value)

inline fun <T> twice(block: () -> T): T {
    block()
    return block()
}
"#;

const CALLER: &str = r#"import kotlin.coroutines.*

suspend fun simple(value: String): String = value

suspend fun echo(value: String): String = applied(value) { simple(it) }

suspend fun collect(): String {
    val parts = StringBuilder()
    twice { parts.append(simple("x")) }
    return parts.toString()
}

suspend fun context(): Boolean {
    twice { if (coroutineContext !== EmptyCoroutineContext) return false }
    return true
}

fun builder(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
}

fun box(): String {
    var result = ""
    builder { result = echo("O") + collect() + context() }
    var inLambda = ""
    builder { twice { inLambda = if (coroutineContext === EmptyCoroutineContext) "K" else "context" } }
    return if (result == "Oxxtrue") "O" + inLambda else "result $result"
}
"#;

#[test]
fn a_suspend_callers_inline_calls_are_inlined_from_the_compiled_body_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[
        ("Declaration.kt", DECLARATION),
        ("Caller.kt", CALLER),
    ]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    // `CallerKt$box$2` differs only in where its line enters around a captured `Ref`'s load, and
    // the library `Continuation { }` copy in its own placement: neither is this route's.
    for name in [
        "CallerKt",
        "CallerKt$collect$1",
        "CallerKt$box$1",
        "DeclarationKt",
    ] {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn a_suspend_callers_inline_calls_inlined_from_the_compiled_body_run() {
    common::expect_box_ok_files_with_stdlib(
        &[("Declaration.kt", DECLARATION), ("Caller.kt", CALLER)],
        "CallerKt",
    );
}

/// The suspensions in the placed lambdas really suspend: each `pause` leaves the machine and
/// resumes it later, in the caller's state, with the lambda's locals spilled around it.
const RESUMING_CALLER: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

var resumed: Continuation<Unit>? = null

suspend fun pause(value: String): String = suspendCoroutineUninterceptedOrReturn { c ->
    resumed = Continuation(c.context) { c.resume(value) }
    COROUTINE_SUSPENDED
}

suspend fun echo(value: String): String = applied(value) { pause(it) }

suspend fun collect(): String {
    var result = ""
    twice { result += pause("x") }
    return result
}

fun builder(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
}

fun drain() {
    while (true) {
        val next = resumed ?: return
        resumed = null
        next.resume(Unit)
    }
}

fun box(): String {
    var result = ""
    builder { result = echo("O") + collect() }
    drain()
    if (result != "Oxx") return "result $result"
    var context = ""
    builder { twice { context = if (coroutineContext === EmptyCoroutineContext) "K" else "context" } }
    return "O" + context
}
"#;

#[test]
fn a_suspension_in_a_placed_lambda_resumes_the_callers_machine() {
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Declaration.kt", DECLARATION),
            ("Caller.kt", RESUMING_CALLER),
        ],
        "CallerKt",
    );
}
