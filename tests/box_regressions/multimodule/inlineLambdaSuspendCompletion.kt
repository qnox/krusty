// MODULE: lib
// FILE: Lib.kt
package lib
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*
var pending: Continuation<String>? = null
suspend fun fetch(s: String): String =
    suspendCoroutineUninterceptedOrReturn { c -> pending = c; COROUTINE_SUSPENDED }
suspend inline fun String.load(path: String): String {
    val got = fetch(this + path)
    return got + path
}
suspend inline fun around(block: suspend () -> String): String = block()
// MODULE: main(lib)
// FILE: Main.kt
import lib.*
import kotlin.coroutines.*
class Api(val base: String) {
    suspend fun direct(path: String): String {
        val trimmed = base.trim()
        val got = trimmed.load(path)
        return got
    }
    suspend fun inLambda(path: String): String {
        val trimmed = base.trim()
        return trimmed.let { value -> value.load(path) }
    }
}
class Placed(val base: String) {
    suspend fun inSuspendLambda(path: String): String {
        val trimmed = base.trim()
        return around { trimmed.load(path) }
    }
}
class Continued(val base: String) {
    suspend fun inSuspendLambda(path: String): String {
        val trimmed = base.trim()
        return around { trimmed.load(path); path }
    }
}
fun drive(block: suspend () -> String): String {
    var r = "none"
    block.startCoroutine(Continuation(EmptyCoroutineContext) { r = it.getOrThrow() })
    while (r == "none") {
        val p = pending ?: return "stuck"
        pending = null
        p.resume("<")
    }
    return r
}
fun box(): String {
    val api = Api(" a ")
    val direct = drive { api.direct("/d") }
    if (direct != "</d") return "direct: $direct"
    val lambda = drive { api.inLambda("/l") }
    if (lambda != "</l") return "lambda: $lambda"
    val suspending = drive { Placed(" a ").inSuspendLambda("/s") }
    if (suspending != "</s") return "suspend lambda: $suspending"
    val continued = drive { Continued(" a ").inSuspendLambda("/p") }
    if (continued != "/p") return "continued suspend lambda: $continued"
    return "OK"
}
