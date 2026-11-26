// MODULE: library
// FILE: Library.kt
package fixture

import kotlin.coroutines.Continuation
import kotlin.coroutines.suspendCoroutine

var parkedAt: Continuation<Int>? = null

suspend fun parked(): Int = suspendCoroutine { parkedAt = it }

suspend fun combined(base: Int): Int {
    val tens = parked()
    val ones = parked()
    return base + tens * 10 + ones
}

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.resume
import kotlin.coroutines.startCoroutine

fun box(): String {
    var result = -1
    val start: suspend () -> Int = { combined(100) }
    start.startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    if (result != -1) return "fail: completed before resuming"
    parkedAt!!.resume(4)
    if (result != -1) return "fail: completed after one resume"
    parkedAt!!.resume(2)
    return if (result == 142) "OK" else "fail: $result"
}
