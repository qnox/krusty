import kotlin.coroutines.Continuation
import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.resume
import kotlin.coroutines.startCoroutine
import kotlin.coroutines.intrinsics.COROUTINE_SUSPENDED
import kotlin.coroutines.intrinsics.suspendCoroutineUninterceptedOrReturn

// A suspend function started through its reference, whose mutable locals a closure and a local
// suspend function share across suspensions. The completion is a source object, not the stdlib's
// `Continuation` builder.

private var parked: Continuation<Int>? = null

private suspend fun pause(): Int = suspendCoroutineUninterceptedOrReturn { continuation ->
    parked = continuation
    COROUTINE_SUSPENDED
}

private suspend fun capturedCell(): Int {
    var total = 2
    val increment = { total += 5 }
    total += pause()
    increment()
    return total
}

private suspend fun capturedByLocalSuspend(): Int {
    var total = 3
    suspend fun increment(): Int {
        total += pause()
        return total
    }
    return increment() + total
}

private class Completion : Continuation<Int> {
    var result: Result<Int>? = null
    override val context: CoroutineContext = EmptyCoroutineContext
    override fun resumeWith(result: Result<Int>) {
        this.result = result
    }
}

private fun run(block: suspend () -> Int, resumed: Int): Int? {
    val completion = Completion()
    block.startCoroutine(completion)
    if (completion.result != null) return -1
    val continuation = parked ?: return -2
    parked = null
    continuation.resume(resumed)
    return completion.result?.getOrNull()
}

fun box(): String {
    val cell = run(::capturedCell, 10)
    if (cell != 17) return "captured cell: $cell"
    val local = run(::capturedByLocalSuspend, 4)
    if (local != 14) return "captured by a local suspend function: $local"
    return "OK"
}
