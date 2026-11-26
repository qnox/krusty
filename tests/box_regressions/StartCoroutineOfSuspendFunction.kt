import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

fun box(): String {
    val block: suspend () -> String = { "OK" }
    val started = block.startCoroutineUninterceptedOrReturn(Continuation(EmptyCoroutineContext) { })
    return if (started === COROUTINE_SUSPENDED) "fail: suspended" else started as String
}
