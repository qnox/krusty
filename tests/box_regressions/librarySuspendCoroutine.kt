import kotlin.coroutines.*

fun box(): String {
    var result: Result<String>? = null
    val block: suspend () -> String = { "OK" }
    block.startCoroutine(object : Continuation<String> {
        override val context: CoroutineContext = EmptyCoroutineContext
        override fun resumeWith(value: Result<String>) {
            result = value
        }
    })
    return result?.getOrThrow() ?: "notResumed"
}
