//! A suspend function VALUE invoked in STATEMENT position mid-body (`f(x); return 1`) — the state
//! machine must give the `InvokeFunction` its own resume state: park on COROUTINE_SUSPENDED and
//! re-enter after resume, not discard the marker and fall through.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Svi")
}

#[test]
fn suspend_value_invoke_statement_then_return() {
    const SRC: &str = "import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
fun runIt(block: suspend () -> Int): Int {\n\
    var res = 0\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { res = it.getOrThrow() })\n\
    return res\n\
}\n\
suspend fun call(f: suspend (String) -> Unit): Int { f(\"A\"); return 1 }\n\
fun box(): String = if (runIt { call { } } == 1) \"OK\" else \"no\"\n";
    let out = run(SRC).expect("mid-body suspend-value invoke should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn suspend_value_invoke_parks_and_resumes() {
    // The value REALLY suspends: it parks its continuation in a global. The coroutine must not
    // complete until that continuation is resumed — a discarded COROUTINE_SUSPENDED would let
    // `call` fall through and complete with 1 immediately (order = "done-before-resume").
    const SRC: &str = "import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
var saved: Continuation<Unit>? = null\n\
var order = \"\"\n\
suspend fun pause(tag: String): Unit = suspendCoroutineUninterceptedOrReturn { c ->\n\
    saved = c\n\
    order += \"parked:\" + tag + \";\"\n\
    COROUTINE_SUSPENDED\n\
}\n\
suspend fun call(f: suspend (String) -> Unit): Int { f(\"A\"); return 1 }\n\
fun box(): String {\n\
    var res = 0\n\
    val block: suspend () -> Int = { call { s -> pause(s) } }\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { res = it.getOrThrow() })\n\
    if (res != 0) return \"fail: completed before resume (res=$res, order=$order)\"\n\
    order += \"resuming;\"\n\
    saved!!.resume(Unit)\n\
    if (res != 1) return \"fail: not completed after resume (res=$res, order=$order)\"\n\
    if (order != \"parked:A;resuming;\") return \"fail order: $order\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("really-suspending value invoke should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn suspend_lambda_with_value_class_params() {
    // Corpus coroutines/inlineClasses/direct/createMangling.kt: a suspend lambda whose parameters
    // are value classes — the erased invoke's boxed args must unbox into the underlying-typed param
    // spill fields (the value-class pass's SetField boundary).
    const SRC: &str = "import kotlin.coroutines.*\n\
fun builder(c: suspend () -> Unit) {\n\
    c.startCoroutine(Continuation(EmptyCoroutineContext) {\n\
        it.getOrThrow()\n\
    })\n\
}\n\
@JvmInline\n\
value class IC(val s: String)\n\
fun box(): String {\n\
    var res = \"FAIL\"\n\
    val lambda: suspend (IC, IC) -> String = { a, b ->\n\
        a.s + b.s\n\
    }\n\
    builder {\n\
        res = lambda(IC(\"O\"), IC(\"K\"))\n\
    }\n\
    return res\n\
}\n";
    let out = run(SRC).expect("value-class-param suspend lambda should compile + run");
    assert_eq!(out, "OK");
}
