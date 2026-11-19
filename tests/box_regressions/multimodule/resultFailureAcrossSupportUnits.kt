// MODULE: lib
// FILE: Lib.kt
package lib
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*
var pending: Continuation<String>? = null
suspend fun fetch(s: String): String =
    suspendCoroutineUninterceptedOrReturn { c -> pending = c; COROUTINE_SUSPENDED }
// MODULE: main(lib)
// FILE: Main.kt
import lib.*
import kotlin.coroutines.*
fun box(): String {
    var r = "none"
    val block: suspend () -> String = { fetch("a") }
    block.startCoroutine(Continuation(EmptyCoroutineContext) { r = it.getOrThrow() })
    val p = pending ?: return "stuck"
    pending = null
    p.resume("OK")
    return r
}
