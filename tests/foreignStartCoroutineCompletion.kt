// WITH_STDLIB

import kotlin.coroutines.Continuation
import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

var done = 0

class Done(override val context: CoroutineContext) : Continuation<Unit> {
    override fun resumeWith(result: Result<Unit>) {
        done += 1
    }
}

fun box(): String {
    val work: suspend () -> Unit = {}
    work.startCoroutine(Done(EmptyCoroutineContext))
    return if (done == 1) "OK" else "FAIL"
}
