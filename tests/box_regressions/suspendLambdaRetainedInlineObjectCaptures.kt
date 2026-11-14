
import kotlin.coroutines.*

abstract class Runner {
    abstract suspend fun run(): String
}

suspend fun foo() {}

inline fun makeRunner(crossinline first: () -> String, crossinline second: () -> String) =
    object : Runner() {
        override suspend fun run(): String {
            foo()
            return first() + second()
        }
    }

object Done : Continuation<Unit> {
    override val context: CoroutineContext get() = EmptyCoroutineContext
    override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }
}

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(Done)
}

fun box(): String {
    var result = "fail"
    val suffix = "K"
    builder {
        val local = "O"
        result = makeRunner({ local }, { suffix }).run()
    }
    return result
}
