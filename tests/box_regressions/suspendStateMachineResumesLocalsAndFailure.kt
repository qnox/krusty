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

private suspend fun immediate(value: Int): Int = value

private fun shouldEnterBranch(value: Int): Boolean {
    events += "c"
    return value > 0
}

private suspend fun resumeThroughBranchAndLoop(): Int {
    var total = immediate(4)
    events += "a"
    val resumed = pause()
    events += "b"
    total += resumed
    if (shouldEnterBranch(resumed)) {
        val branchValue = pause()
        total += branchValue
        events += "d"
        var index = 0
        while (index < 2) {
            val loopValue = pause()
            total += loopValue + index
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
    if (events != "abc" || completed != null) return "branch condition resume: $events"
    val second = parked ?: return "missing loop continuation"
    parked = null
    second.resume(1)
    if (events != "abcd" || completed != null) return "branch-body resume: $events"
    val third = parked ?: return "missing second loop continuation"
    parked = null
    third.resume(5)
    if (events != "abcdl" || completed != null) return "first loop resume: $events"
    val fourth = parked ?: return "missing second loop continuation"
    parked = null
    fourth.resume(7)
    if (events != "abcdll") return "resume path: $events"
    if (completed?.getOrNull() != 21) return "saved local/result"

    completed = null
    ::resumeFailure.startCoroutine(
        Continuation(EmptyCoroutineContext) { completed = it },
    )
    val failureContinuation = parked ?: return "missing failure continuation"
    parked = null
    failureContinuation.resumeWith(Result.failure(IllegalStateException("resumed failure")))
    val failure = completed?.exceptionOrNull()
    if (failure?.message != "resumed failure") return "failure propagation"
    return "OK"
}
