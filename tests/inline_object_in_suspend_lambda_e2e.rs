//! An inline function's anonymous object whose suspend member calls a `crossinline` lambda,
//! created inside a suspend lambda. The lambda is spliced into the object's member, so its
//! implementation has no body left; the object's copy for the call site still names that consumed
//! implementation, and its copy has no method of its own to emit.
//!
//! kotlinc 2.4 writes classes for this program that fail verification (KT-86095); the reference
//! comparison is the 2.5 corpus case `coroutines/kt86095.kt`. This pins krusty's own run.

use super::common;

const PROGRAM: &str = r#"
import kotlin.coroutines.*

abstract class Runner {
    internal abstract suspend fun run(): String
}

suspend fun suspended() {}

inline fun makeRunner(crossinline value: () -> String) =
    object : Runner() {
        override suspend fun run(): String {
            suspended()
            return value()
        }
    }

fun builder(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
}

fun box(): String {
    var result = "fail"
    builder {
        result = makeRunner { "O" }.run() + makeRunner { "K" }.run()
    }
    return result
}
"#;

#[test]
fn an_inline_object_in_a_suspend_lambda_keeps_its_consumed_lambda_consumed() {
    assert_eq!(
        common::expect_box_run_with_stdlib(PROGRAM, "InlineObjectInSuspendLambda"),
        "OK"
    );
}
