// A Unit suspend function that is a bare suspendCoroutine must still suspend its caller.
// WITH_STDLIB
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

var saved: Continuation<Unit>? = null
var stage = ""

suspend fun wait(): Unit = suspendCoroutine { c -> saved = c }

fun box(): String {
    var stopped = false
    val body: suspend () -> Unit = {
        stage += "a"
        wait()
        stage += "b"
    }
    body.startCoroutine(Continuation(EmptyCoroutineContext) { stopped = true })
    if (stopped) return "completed early: $stage"
    if (stage != "a") return "stage $stage"
    saved!!.resume(Unit)
    if (!stopped) return "not resumed: $stage"
    return if (stage == "ab") "OK" else stage
}
