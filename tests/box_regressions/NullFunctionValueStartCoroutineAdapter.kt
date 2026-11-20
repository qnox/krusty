// KOTLINC_VERSION: (,2.5.0)
// Before Kotlin 2.5 a null value of a nullable function type converted to a nullable suspend
// function type becomes an adapter: calling it yields null, or, for a type parameter with a
// nullable function bound, throws NullPointerException.
// Each form enters a coroutine through `startCoroutine` and observes its completion through
// `Result.fold`. NullFunctionValueStartCoroutine.kt is the same program with the other releases'
// observations; NullFunctionValueSuspendConversion*.kt observe the same forms through a direct
// call.
import kotlin.coroutines.*

suspend fun observe(f: (suspend () -> String?)?): String = if (f == null) "null" else "adapter:" + f()
suspend fun observeUnit(f: (suspend () -> Unit)?): String = if (f == null) "null" else { f(); "adapter" }

suspend fun parameter(f: (() -> String?)?) = observe(f)
suspend fun <F : (() -> String?)?> typeParameter(f: F) = observe(f)
fun nothing(): (() -> String?)? = null
suspend fun callResult() = observe(nothing())
suspend fun localVar(): String {
    var f: (() -> String?)? = null
    return observe(f)
}
suspend fun unitParameter(f: (() -> Unit)?) = observeUnit(f)

fun run(block: suspend () -> String): String {
    var result: Result<String>? = null
    block.startCoroutine(Continuation(EmptyCoroutineContext) { result = it })
    return result!!.fold({ it }, { if (it is NullPointerException) "NPE" else "other" })
}

fun observations(): String = listOf(
    run { parameter(null) },
    run { typeParameter(null) },
    run { callResult() },
    run { localVar() },
    run { unitParameter(null) },
).joinToString(",")

fun box(): String {
    val observed = observations()
    return if (observed == "adapter:null,NPE,adapter:null,adapter:null,adapter") "OK" else "fail: $observed"
}
