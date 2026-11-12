//! An inline function's anonymous object, built inside a suspend lambda, may keep a lambda
//! argument of the call as a field. The suspend lambda class must not try to emit that lambda as
//! a method of its own: the inliner already consumed its body.

use super::common;

const CROSSINLINE_LAMBDA_IN_SUSPEND_LAMBDA: &str = r#"
import kotlin.coroutines.*

abstract class Runner {
    abstract suspend fun run(): String
}

suspend fun foo() {}

inline fun makeRunner(crossinline f: () -> String) =
    object : Runner() {
        override suspend fun run(): String {
            foo()
            return f()
        }
    }

object Done : Continuation<Unit> {
    override val context: CoroutineContext get() = EmptyCoroutineContext
    override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }
}

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(Done)
}

fun box(): String {
    var result = "fail"
    builder {
        result = makeRunner { "OK" }.run()
    }
    return result
}
"#;

const TWO_LAMBDAS_IN_SUSPEND_LAMBDA: &str = r#"
import kotlin.coroutines.*

abstract class Runner {
    abstract suspend fun run(): String
}

suspend fun foo() {}

inline fun makeRunner(crossinline first: () -> String, crossinline second: () -> String) =
    object : Runner() {
        override suspend fun run(): String {
            foo()
            return first() + second()
        }
    }

object Done : Continuation<Unit> {
    override val context: CoroutineContext get() = EmptyCoroutineContext
    override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }
}

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(Done)
}

fun box(): String {
    var result = "fail"
    val suffix = "K"
    builder {
        val local = "O"
        result = makeRunner({ local }, { suffix }).run()
    }
    return result
}
"#;

#[test]
fn a_lambda_kept_by_an_inlined_object_does_not_become_a_method_of_the_suspend_lambda() {
    common::expect_box_ok_with_stdlib(CROSSINLINE_LAMBDA_IN_SUSPEND_LAMBDA, "SuspendInlineObject");
}

#[test]
fn lambdas_capturing_values_of_the_suspend_lambda_are_kept_by_the_inlined_object() {
    common::expect_box_ok_with_stdlib(TWO_LAMBDAS_IN_SUSPEND_LAMBDA, "SuspendInlineObjectCaptures");
}
