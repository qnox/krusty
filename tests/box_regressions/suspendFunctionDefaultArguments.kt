// A suspend function called with omitted arguments: the defaults run in the callee's frame (one
// reads an earlier parameter), the call suspends, and the caller resumes with the result.
// WITH_STDLIB
import kotlin.coroutines.*

var saved: Continuation<String>? = null

suspend fun pause(value: String): String = suspendCoroutine { c ->
    saved = c
    log += value
}

var log = ""

suspend fun greet(name: String, greeting: String = "hi $name", times: Int = 2): String {
    val first = pause(greeting.repeat(times))
    return first + name
}

class Counter(val base: Int) {
    suspend fun add(step: Int = base + 1): Int {
        pause("step$step")
        return base + step
    }
}

fun <T> run(block: suspend () -> T): T {
    var result: Result<T>? = null
    block.startCoroutine(Continuation(EmptyCoroutineContext) { result = it })
    saved!!.resume("r")
    return result!!.getOrThrow()
}

fun box(): String {
    if (run { greet("k") } != "rk") return "greet ${log}"
    if (log != "hi khi k") return "log $log"
    if (run { greet("k", times = 1) } != "rk") return "times"
    if (run { Counter(3).add() } != 7) return "member"
    if (log != "hi khi khi kstep4") return "member log $log"
    return "OK"
}
