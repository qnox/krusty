
import kotlin.coroutines.*

abstract class Runner {
    abstract suspend fun run(): String
}

suspend fun foo() {}

inline fun makeRunner(crossinline f: () -> String) =
    object : Runner() {
        override suspend fun run(): String {
            foo()
            return f()
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
    builder {
        result = makeRunner { "OK" }.run()
    }
    return result
}
