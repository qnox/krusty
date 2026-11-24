// WITH_STDLIB

import kotlin.coroutines.*

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(Continuation(EmptyCoroutineContext) {
        it.getOrThrow()
    })
}

var pending: Continuation<Any>? = null

fun box(): String {
    var res = ""
    builder {
        val first = suspendCoroutine<String> { cont ->
            @Suppress("UNCHECKED_CAST")
            pending = cont as Continuation<Any>
        }
        res += first
        res += suspendCoroutine<String> { cont ->
            @Suppress("UNCHECKED_CAST")
            pending = cont as Continuation<Any>
        }
    }
    pending?.resume("O")
    pending?.resume("K")
    return res
}
