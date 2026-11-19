// WITH_STDLIB
// A library value class (`Result`) that one file's support unit already materializes is reused by
// a later file's support unit, not materialized a second time: the helper file reads
// `Result.exceptionOrNull`, and the main file's `startCoroutine` reaches the same class through
// the continuation it resumes with a `Result`.
// FILE: helper.kt
import kotlin.coroutines.*

fun failureOf(result: Result<Int>): Throwable? = result.exceptionOrNull()

// FILE: main.kt
import kotlin.coroutines.*

fun run(c: suspend () -> Int): Result<Int> {
    var out: Result<Int>? = null
    c.startCoroutine(object : Continuation<Int> {
        override val context: CoroutineContext = EmptyCoroutineContext
        override fun resumeWith(result: Result<Int>) {
            out = result
        }
    })
    return out!!
}

fun box(): String {
    val failure = IllegalStateException("boom")
    if (failureOf(Result.failure(failure)) !== failure) return "failure"
    if (failureOf(Result.success(1)) != null) return "success"
    val done = run { 41 + 1 }
    return if (done.getOrNull() == 42 && failureOf(done) == null) "OK" else "FAIL"
}
