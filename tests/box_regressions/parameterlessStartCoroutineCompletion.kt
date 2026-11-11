// WITH_STDLIB

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

var completions = 0

fun box(): String {
    val work: suspend () -> Unit = {}
    work.startCoroutine(Continuation<Unit>(EmptyCoroutineContext) { completions += 1 })
    return if (completions == 1) "OK" else "FAIL"
}
