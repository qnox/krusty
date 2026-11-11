// WITH_STDLIB

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

var order = 0

fun work(): suspend () -> Unit {
    order = order * 10 + 1
    return {}
}

fun completion(): Continuation<Unit> {
    order = order * 10 + 2
    return Continuation(EmptyCoroutineContext) {}
}

fun box(): String {
    work().startCoroutine(completion())
    return if (order == 12) "OK" else "FAIL"
}
