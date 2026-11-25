// A labeled return from an inlined Unit lambda that also suspends leaves that lambda's frame and
// continues with the next iteration of the inlined `repeat`, whether the return precedes the
// suspension, follows it, or the suspension is the lambda's last statement.
import kotlin.coroutines.*

var log = ""

suspend fun mark(value: String): String? {
    log += value
    return null
}

suspend fun before() {
    repeat(3) {
        if (it == 1) return@repeat
        mark("b$it")
    }
}

suspend fun after() {
    repeat(3) {
        if (it == 1) {
            mark("a$it")
            return@repeat
        }
        log += "n$it"
    }
}

suspend fun unitLast() {
    repeat(2) {
        if (it == 0) return@repeat
        mark("u$it")
    }
}

fun box(): String {
    var result = ""
    suspend {
        before()
        after()
        unitLast()
    }.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow(); result = log })
    return if (result == "b0b2n0a1n2u1") "OK" else "fail: $result"
}
