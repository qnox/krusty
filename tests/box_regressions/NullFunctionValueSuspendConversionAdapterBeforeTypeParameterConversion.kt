// KOTLINC_VERSION: (,2.4.20)
// Before Kotlin 2.5 a null value of a nullable function type converted to a nullable suspend
// function type becomes an adapter: calling it yields null. A type parameter with a nullable
// function bound is converted from 2.4.20 on (where calling it throws NullPointerException); until
// then its null value stays null. NullFunctionValueSuspendConversionAdapter.kt is the same program
// from 2.4.20; Kotlin 2.5 keeps every null value null (KT-89451), see
// NullFunctionValueSuspendConversion.kt.
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
    return if (observed == "adapter:null,null,adapter:null,adapter:null,adapter") "OK" else "fail: $observed"
}
