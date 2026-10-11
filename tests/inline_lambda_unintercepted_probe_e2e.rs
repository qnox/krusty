//! A `suspendCoroutineUninterceptedOrReturn` block reached through an inline suspend function
//! inlined into a lambda that is itself inlined into a suspend function suspends that function's
//! own machine. kotlinc's debug probe after the block reports the suspension with the machine's
//! continuation, the same one the block was given. The inline function the lambda is passed to is
//! this repository's own, compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <R> runIt(block: () -> R): R = block()
"#;

const MAIN: &str = r#"
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

inline suspend fun resumeWith(value: String): String =
    suspendCoroutineUninterceptedOrReturn { continuation ->
        continuation.resume(value)
        COROUTINE_SUSPENDED
    }

suspend fun resumed(): String = runIt { resumeWith("O") } + "K"

fun box(): String {
    var result = "FAIL"
    suspend { resumed() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    return result
}
"#;

#[test]
fn an_unintercepted_block_in_an_inlined_lambda_resumes_its_machine() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}
