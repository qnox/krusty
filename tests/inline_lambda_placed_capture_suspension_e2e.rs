//! A suspension in a literal lambda that an inline function only invokes from inside another
//! inlined lambda suspends the enclosing coroutine. Expanding the function leaves the literal as a
//! capture of the inner lambda, and its body is placed at each invocation in the caller's frame, so
//! that frame's machine owns the suspension, and a non-local return from it leaves the coroutine.
//! The inner inline function is this repository's own, compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <R> runIt(block: () -> R): R = block()
"#;

const MAIN: &str = r#"
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

class Controller {
    var cResult = 0
    suspend fun suspendHere(v: Int): Int = suspendCoroutineUninterceptedOrReturn { x ->
        x.resume(v * 2)
        COROUTINE_SUSPENDED
    }
}

fun builder(c: suspend Controller.() -> Int): Controller {
    val controller = Controller()
    c.startCoroutine(controller, Continuation(EmptyCoroutineContext) {
        controller.cResult = it.getOrThrow()
    })
    return controller
}

inline fun foo(x: (Int) -> Unit) {
    for (i in 1..2) {
        runIt { x(i) }
    }
}

fun box(): String {
    var result = ""
    val controllerResult = builder {
        result += "-"
        foo {
            runIt {
                result += suspendHere(it).toString()
                if (it == 2) return@builder 56
            }
        }
        result += "+"
        1
    }.cResult
    if (result != "-24") return "fail: $result"
    if (controllerResult != 56) return "fail2: $controllerResult"
    return "OK"
}
"#;

#[test]
fn a_placed_capture_suspends_the_enclosing_coroutine() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}
