//! The remaining `Unit` tails of a SUSPENDING lambda body that leave nothing on the operand stack.
//! A tail bound to the state machine's result temp must be a value; a void one has to materialize the
//! `Unit` singleton first, or `invokeSuspend` stores from an empty stack (`VerifyError: Operand stack
//! underflow`). The direct call/`try` spellings are covered in `suspend_receiver_lambda_e2e`; these two
//! reach the same emit through a different node — a SAFE CALL (checked type `Unit?`, not `Unit`) and an
//! inline-SPLICED call (the value sits in a nested block).

use super::common;

fn run(src: &str, stem: &str) {
    let Some(got) = common::compile_and_run_with_stdlib(src, stem) else {
        panic!("{stem}: expected the box to compile and run");
    };
    assert_eq!(got, "OK", "{stem}");
}

/// `h?.act()` — a safe call on a `Unit` member. The whole expression is typed `Unit?`.
#[test]
fn suspend_lambda_safe_call_unit_tail() {
    run(
        r#"
import kotlin.coroutines.*

suspend fun work(): Int = 5

var out = ""

class Holder {
    fun act() {
        out += "h"
    }
}

val h: Holder? = Holder()

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    builder {
        val a = work()
        out += a
        h?.act()
    }
    return if (out == "5h") "OK" else "FAIL $out"
}
"#,
        "SafeTailKt",
    );
}

/// A `null` receiver takes the other arm of the same safe call — the tail still yields `Unit`.
#[test]
fn suspend_lambda_safe_call_unit_tail_null_receiver() {
    run(
        r#"
import kotlin.coroutines.*

suspend fun work(): Int = 5

var out = ""

class Holder {
    fun act() {
        out += "h"
    }
}

val h: Holder? = null

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    builder {
        val a = work()
        out += a
        h?.act()
    }
    return if (out == "5") "OK" else "FAIL $out"
}
"#,
        "SafeTailNullKt",
    );
}

/// The same present/null safe-call tails without an earlier suspension use the leaf `invokeSuspend`
/// path. Its result coercion must share the general state machine's `Unit?` rule; otherwise it tries to
/// box the void safe-call IR and emits `areturn` with an empty operand stack.
#[test]
fn suspend_lambda_leaf_safe_call_unit_tails() {
    run(
        r#"
import kotlin.coroutines.*

var out = ""

class Holder {
    fun act() {
        out += "h"
    }
}

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    val present: Holder? = Holder()
    val absent: Holder? = null
    builder { present?.act() }
    builder { absent?.act() }
    return if (out == "h") "OK" else "FAIL $out"
}
"#,
        "LeafSafeTailsKt",
    );
}

/// A nullable-`Unit` safe call nested below a checked conversion and then consumed by another
/// expression. The branch suspension must be split into states inside the selected branch; the
/// outer consumer runs only after the coroutine resumes.
#[test]
fn suspend_lambda_nested_unit_safe_call_resumes_before_outer_consumer() {
    run(
        r#"
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

var pending: Continuation<Unit>? = null
var observed = "FAIL"

suspend fun pause(): Unit = suspendCoroutineUninterceptedOrReturn {
    pending = it
    COROUTINE_SUSPENDED
}

class Holder {
    suspend fun act() = pause()
}

val Any?.tag: String
    get() = if (this == null) "null" else "Unit"

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
}

fun box(): String {
    val holder: Holder? = Holder()
    builder {
        observed = holder?.act().tag
        Unit
    }
    if (observed != "FAIL") return "completed before resume: $observed"
    pending!!.resume(Unit)
    return if (observed == "Unit") "OK" else observed
}
"#,
        "NestedSafeUnitKt",
    );
}

/// A non-suspending void tail call whose ARGUMENT suspends must still be materialized. Argument
/// evaluation is lowered ahead of the call and remains visible to the coroutine flattener; treating
/// every call subtree containing suspension as a suspending tail would incorrectly leave the outer
/// void call unwrapped and reintroduce the empty-stack store after the argument resumes.
#[test]
fn suspend_lambda_void_tail_with_suspending_argument() {
    run(
        r#"
import kotlin.coroutines.*

suspend fun value(): Int = 5

var out = 0

fun sink(value: Int) {
    out = value
}

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    builder { sink(value()) }
    return if (out == 5) "OK" else "FAIL $out"
}
"#,
        "SuspendingArgumentTailKt",
    );
}

/// `run { … }` — an inline-SPLICED `Unit` tail, whose value ends up in a nested block.
#[test]
fn suspend_lambda_inline_spliced_unit_tail() {
    run(
        r#"
import kotlin.coroutines.*

suspend fun work(): Int = 5

var out = ""

fun sink(x: Int) {
    out += x
}

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    builder {
        val a = work()
        run { sink(a) }
    }
    return if (out == "5") "OK" else "FAIL $out"
}
"#,
        "SpliceTailKt",
    );
}
