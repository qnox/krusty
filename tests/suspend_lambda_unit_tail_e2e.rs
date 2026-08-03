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
