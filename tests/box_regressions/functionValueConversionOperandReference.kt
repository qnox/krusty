import kotlin.coroutines.*

fun runSuspend(c: suspend Int.() -> Unit) {
    c.startCoroutine(1, Continuation(EmptyCoroutineContext) { it.getOrThrow() })
}

var test = "failed"

val a: Int.() -> Unit = { test = "OK" }

fun box(): String {
    runSuspend(::a.get())
    return test
}
