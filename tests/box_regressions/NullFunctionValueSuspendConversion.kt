// KOTLINC_VERSION: [2.5.0,)
// A null value of a nullable function type converted to a nullable suspend function type stays
// null: a parameter, a type parameter with a nullable function bound, a call result and a local
// `var`, with a value result and a `Unit` result (KT-89451). Releases before 2.5 convert it to an
// adapter instead; NullFunctionValueSuspendConversionAdapter.kt pins those observations.
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

// A context with no elements, and the continuation every call is handed: nothing here suspends,
// so each call completes in place and never resumes it.
object NoElements : CoroutineContext {
    override fun <E : CoroutineContext.Element> get(key: CoroutineContext.Key<E>): E? = null
    override fun <R> fold(initial: R, operation: (R, CoroutineContext.Element) -> R): R = initial
    override fun minusKey(key: CoroutineContext.Key<*>): CoroutineContext = this
}

object NeverResumed : Continuation<Any?> {
    override val context: CoroutineContext get() = NoElements
    override fun resumeWith(result: Result<Any?>) {
        throw IllegalStateException("a call that completes in place resumed its continuation")
    }
}

@Suppress("UNCHECKED_CAST")
fun run(block: suspend () -> String): Any? =
    try {
        (block as (Continuation<Any?>) -> Any?)(NeverResumed)
    } catch (e: NullPointerException) {
        "NPE"
    }

fun observations(): String =
    "${run { parameter(null) }}" + "," +
        "${run { typeParameter(null) }}" + "," +
        "${run { callResult() }}" + "," +
        "${run { localVar() }}" + "," +
        "${run { unitParameter(null) }}"

fun box(): String {
    val observed = observations()
    return if (observed == "null,null,null,null,null") "OK" else "fail: $observed"
}
