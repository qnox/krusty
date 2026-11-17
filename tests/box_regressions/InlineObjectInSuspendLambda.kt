// KOTLINC_VERSION: [2.5.0,)
// kotlinc before 2.5 miscompiles this program (KT-86095), so it cannot be the oracle.
import kotlin.coroutines.*

abstract class Runner {
    internal abstract suspend fun run(): String
}

suspend fun suspended() {}

inline fun makeRunner(crossinline value: () -> String) =
    object : Runner() {
        override suspend fun run(): String {
            suspended()
            return value()
        }
    }

fun builder(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
}

fun box(): String {
    var result = "fail"
    builder {
        result = makeRunner { "O" }.run() + makeRunner { "K" }.run()
    }
    return result
}
