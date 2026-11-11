// WITH_STDLIB
// A continuation allocated in another file of the same module is that interface, and starting a
// coroutine on it delivers the completion.
// FILE: made.kt

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext

var delivered = 0

fun made(): Continuation<Unit> = Continuation(EmptyCoroutineContext) { delivered += 1 }

// FILE: main.kt

import kotlin.coroutines.Continuation
import kotlin.coroutines.startCoroutine

fun box(): String {
    val c = made()
    val member = (c as Any) is Continuation<*>
    val work: suspend () -> Unit = {}
    work.startCoroutine(c)
    return if (member && delivered == 1) "OK" else "FAIL"
}
