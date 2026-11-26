// WITH_STDLIB
// WITH_COROUTINES
// The support unit that materializes `IntRange` for the lambda's state machine also materializes
// the library enum `CoroutineSingletons`. That enum's synthetic `entries` body calls
// `enumEntries`, whose list reaches `IntRange.contains`: the unit lowers the call with the class,
// rather than a later unit calling into a class this one is still materializing.
import helpers.*
import kotlin.coroutines.*

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(EmptyContinuation)
}

fun box(): String {
    var sum = 0
    builder {
        val it = (1..10).iterator()
        while (it.hasNext()) sum += it.next()
    }
    return if (sum == 55) "OK" else "FAIL: $sum"
}
