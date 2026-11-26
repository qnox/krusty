// WITH_STDLIB

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

class Settings {
    var retries = 0
}

class Client(val settings: Settings)

// The receiver of `apply` is stored once for the inlined call; the lambda's receiver reads it, so
// the local that follows takes the next slot.
fun configured(): String {
    val settings = Settings().apply { retries = 2 }
    val client = Client(settings)
    return if (client.settings.retries == 2) "O" else "fail ${client.settings.retries}"
}

fun run(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    var result = "not run"
    run {
        val settings = Settings().apply { retries = 3 }
        val client = Client(settings)
        result = if (client.settings.retries == 3) configured() + "K" else "fail"
    }
    return result
}
