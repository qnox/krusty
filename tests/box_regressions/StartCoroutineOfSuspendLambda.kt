import kotlin.coroutines.*

fun box(): String {
    var result = "fail"
    val block: suspend () -> String = { "OK" }
    block.startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    return result
}
