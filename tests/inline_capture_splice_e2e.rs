//! An inline lambda passed through stdlib `run` is still spliced.
//!
//! `run` copies each non-shared capture into an unnamed temporary and invokes that temporary.
//! When the capture is an inline lambda, the temporary's initializer is the lambda, but the
//! invocation reads the temporary. A non-local return makes the lambda inline-only, so leaving
//! the temporary in place calls a method that is never emitted. The same shape with a suspension
//! inside the lambda is `coroutines/nonLocalReturnFromInlineLambdaDeep.kt`.

use super::common;

#[test]
fn non_local_return_through_stdlib_run_inside_inline() {
    const SRC: &str = "\
inline fun foo(x: (Int) -> Unit) {\n\
    for (i in 1..2) {\n\
        run { x(i) }\n\
    }\n\
}\n\
fun box(): String {\n\
    foo { if (it == 2) return \"OK\" }\n\
    return \"FAIL\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "InlineRunNonLocal");
}

#[test]
fn inline_lambda_value_through_stdlib_run_still_runs() {
    const SRC: &str = "\
inline fun foo(x: (Int) -> Int): Int {\n\
    var sum = 0\n\
    for (i in 1..3) {\n\
        run { sum += x(i) }\n\
    }\n\
    return sum\n\
}\n\
fun box(): String = if (foo { it * it } == 14) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(SRC, "InlineRunValue");
}

#[test]
fn suspended_non_local_return_through_nested_run() {
    // Corpus `coroutines/nonLocalReturnFromInlineLambdaDeep.kt`, with the test helper's
    // completion written as a direct `Continuation`.
    const SRC: &str = "\
import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
\n\
class Controller {\n\
    var cResult = 0\n\
    suspend fun suspendHere(v: Int): Int = suspendCoroutineUninterceptedOrReturn { x ->\n\
        x.resume(v * 2)\n\
        COROUTINE_SUSPENDED\n\
    }\n\
}\n\
\n\
fun builder(c: suspend Controller.() -> Int): Controller {\n\
    val controller = Controller()\n\
    c.startCoroutine(controller, Continuation(EmptyCoroutineContext) {\n\
        controller.cResult = it.getOrThrow()\n\
    })\n\
    return controller\n\
}\n\
\n\
inline fun foo(x: (Int) -> Unit) {\n\
    for (i in 1..2) {\n\
        run { x(i) }\n\
    }\n\
}\n\
\n\
fun box(): String {\n\
    var result = \"\"\n\
    val controllerResult = builder {\n\
        result += \"-\"\n\
        foo {\n\
            run {\n\
                result += suspendHere(it).toString()\n\
                if (it == 2) return@builder 56\n\
            }\n\
        }\n\
        result += \"+\"\n\
        1\n\
    }.cResult\n\
    if (result != \"-24\") return \"fail 1: $result\"\n\
    if (controllerResult != 56) return \"fail 2: $controllerResult\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "InlineRunSuspendReturn");
}
