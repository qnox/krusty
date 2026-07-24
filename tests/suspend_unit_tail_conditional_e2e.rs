//! A `Unit` suspend lambda/fn whose LAST statement is an `if`/`when` containing the suspension
//! (`builder { if (suspendHere() != "OK") throw … }`, the corpus `coroutines/emptyClosure.kt`
//! shape). The lambda body reaches the state machine as `return <statement-shaped When>` — its
//! value emission leaves nothing on the stack, underflowing the return's spill (VerifyError).
//! `split_unit_conditional_returns` rewrites it to `<when as stmt>; return Unit.INSTANCE`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

const PRELUDE: &str = "import kotlin.coroutines.*\n\
object EC : Continuation<Unit> {\n\
    override val context: CoroutineContext = EmptyCoroutineContext\n\
    override fun resumeWith(result: Result<Unit>) {}\n\
}\n";

#[test]
fn suspend_lambda_tail_if_throw() {
    let src = format!(
        "{PRELUDE}var log = 0\n\
suspend fun sh(): String {{ log = log + 1; return \"OK\" }}\n\
fun builder(c: suspend () -> Unit) {{ c.startCoroutine(EC) }}\n\
fun box(): String {{\n\
    builder {{ if (sh() != \"OK\") throw RuntimeException(\"fail 1\") }}\n\
    return if (log == 1) \"OK\" else \"fail $log\"\n\
}}\n"
    );
    assert_eq!(run(&src).expect("compiles + runs"), "OK");
}

#[test]
fn suspend_lambda_tail_if_plain_branch() {
    let src = format!(
        "{PRELUDE}var log = 0\n\
suspend fun sh(): String {{ log = log + 1; return \"OK\" }}\n\
fun builder(c: suspend () -> Unit) {{ c.startCoroutine(EC) }}\n\
fun box(): String {{\n\
    builder {{ if (sh() != \"OK\") log = 99 }}\n\
    return if (log == 1) \"OK\" else \"fail $log\"\n\
}}\n"
    );
    assert_eq!(run(&src).expect("compiles + runs"), "OK");
}

#[test]
fn suspend_receiver_lambda_tail_if_throw() {
    // The receiver form (`suspend Controller.() -> Unit`) — the corpus emptyClosure.kt shape.
    let src = format!(
        "{PRELUDE}var log = 0\n\
class Controller {{\n\
    suspend fun suspendHere(): String {{ log = log + 1; return \"OK\" }}\n\
}}\n\
fun builder(c: suspend Controller.() -> Unit) {{ c.startCoroutine(Controller(), EC) }}\n\
fun box(): String {{\n\
    for (i in 1..3) {{\n\
        builder {{ if (suspendHere() != \"OK\") throw RuntimeException(\"fail 1\") }}\n\
    }}\n\
    return if (log == 3) \"OK\" else \"fail $log\"\n\
}}\n"
    );
    assert_eq!(run(&src).expect("compiles + runs"), "OK");
}

#[test]
fn suspend_fun_tail_if_throw() {
    // Same shape in a named `Unit` suspend FUNCTION (the non-lambda state machine path).
    let src = format!(
        "{PRELUDE}var log = 0\n\
suspend fun sh(): String {{ log = log + 1; return \"OK\" }}\n\
suspend fun check() {{ if (sh() != \"OK\") throw RuntimeException(\"fail 1\") }}\n\
fun builder(c: suspend () -> Unit) {{ c.startCoroutine(EC) }}\n\
fun box(): String {{\n\
    builder {{ check(); check() }}\n\
    return if (log == 2) \"OK\" else \"fail $log\"\n\
}}\n"
    );
    assert_eq!(run(&src).expect("compiles + runs"), "OK");
}
