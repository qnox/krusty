// A suspend caller that inlines a dependency's body invoking a function value the call passes
// without inlining suspends at that invoke, and resumes with the value's result.
// MODULE: lib
// FILE: Direct.kt
package lib

suspend inline fun twiceOf(block: suspend () -> Int): Int = listOf(block()).first() * 2
// MODULE: main(lib)
// FILE: main.kt
import lib.*
import kotlin.coroutines.*

suspend fun viaValue(step: suspend () -> Int): Int = twiceOf(step)

var pending: Continuation<Int>? = null

suspend fun later(): Int = suspendCoroutine { pending = it }

fun box(): String {
    var result: Any? = null
    suspend { viaValue { later() } }.startCoroutine(Continuation(EmptyCoroutineContext) {
        result = it.getOrThrow()
    })
    if (result != null) return "FAIL completed before resuming: $result"
    pending!!.resume(21)
    return if (result == 42) "OK" else "FAIL resumed: $result"
}
