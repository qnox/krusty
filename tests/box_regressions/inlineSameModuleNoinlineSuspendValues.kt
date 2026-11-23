// A same-module inline function with a `noinline` suspend function-typed parameter passes the
// parameter's argument on as a value, so the call's suspend lambda is the call site's own object
// and nothing in the callee suspends inside the caller.
// FILE: Declaration.kt
inline fun <T> deferredBy(noinline block: suspend () -> T): suspend () -> T = block

inline fun <T> pairedWith(value: T, noinline block: suspend (T) -> T): suspend () -> T = { block(value) }
// FILE: Caller.kt
import kotlin.coroutines.*

suspend fun pause(value: Int): Int = suspendCoroutine { it.resume(value) }

fun later(): suspend () -> Int = deferredBy { pause(20) + 1 }

suspend fun now(): Int = deferredBy { pause(1) }() + pairedWith(2) { pause(it) * 10 }()

fun builder(block: suspend () -> Unit) {
    block.startCoroutine(object : Continuation<Unit> {
        override val context: CoroutineContext get() = EmptyCoroutineContext
        override fun resumeWith(result: Result<Unit>) {
            result.getOrThrow()
        }
    })
}

fun box(): String {
    var result = 0
    builder { result = later()() + now() }
    return if (result == 42) "OK" else "result $result"
}
