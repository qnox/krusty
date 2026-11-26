// A library suspend function suspends inside its expression body on a function value it receives.
// Its machine keeps the completion it resumes apart from the value the suspension delivers, even
// when the library module declares no other locals.
// MODULE: lib
// FILE: Lib.kt
package lib

suspend fun twiceOf(block: suspend () -> Int): Int = listOf(block()).first() * 2
// MODULE: main(lib)
// FILE: main.kt
import lib.*
import kotlin.coroutines.*

var pending: Continuation<Int>? = null

suspend fun later(): Int = suspendCoroutine { pending = it }

fun box(): String {
    var result: Any? = null
    suspend { twiceOf { later() } }.startCoroutine(Continuation(EmptyCoroutineContext) {
        result = it.getOrThrow()
    })
    if (result != null) return "FAIL completed before resuming: $result"
    pending!!.resume(21)
    return if (result == 42) "OK" else "FAIL resumed: $result"
}
