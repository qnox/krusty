import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.resume
import kotlin.coroutines.startCoroutine
import kotlin.coroutines.intrinsics.COROUTINE_SUSPENDED
import kotlin.coroutines.intrinsics.suspendCoroutineUninterceptedOrReturn

private var parked: Continuation<Int>? = null
private var events = ""

private suspend fun pause(): Int = suspendCoroutineUninterceptedOrReturn { continuation ->
    parked = continuation
    COROUTINE_SUSPENDED
}

private suspend fun resumeThroughBranchAndLoop(): Int {
    var total = 4
    events += "a"
    val resumed = pause()
    events += "b"
    if (resumed > 0) {
        events += "c"
        var index = 0
        while (index < 2) {
            total += resumed + index
            events += "l"
            index += 1
        }
    } else {
        events += "x"
    }
    return total
}

private suspend fun resumeFailure(): Int {
    pause()
    return 99
}

fun box(): String {
    var completed: Result<Int>? = null
    ::resumeThroughBranchAndLoop.startCoroutine(
        Continuation(EmptyCoroutineContext) { completed = it },
    )
    if (events != "a" || completed != null) return "did not suspend"
    val first = parked ?: return "missing continuation"
    parked = null
    first.resume(3)
    if (events != "abcll") return "resume path: $events"
    if (completed?.getOrNull() != 11) return "saved local/result"

    completed = null
    ::resumeFailure.startCoroutine(
        Continuation(EmptyCoroutineContext) { completed = it },
    )
    val second = parked ?: return "missing failure continuation"
    parked = null
    second.resumeWith(Result.failure(IllegalStateException("resumed failure")))
    val failure = completed?.exceptionOrNull()
    if (failure?.message != "resumed failure") return "failure propagation"
    return "OK"
}
