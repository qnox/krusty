// A value of a nullable regular function type passed where a nullable suspend function type is
// expected is suspend-converted like a non-null one, with no parameter, with parameters, with an
// extension receiver, and with a `Unit` result. Each value here is non-null.
import kotlin.coroutines.*

suspend fun none(f: (suspend () -> String)?): String = if (f == null) "null" else f()
suspend fun two(f: (suspend (Int, String) -> String)?): String = if (f == null) "null" else f(1, "a")
suspend fun extension(f: (suspend Int.() -> String)?): String = if (f == null) "null" else 2.f()
suspend fun effect(f: (suspend () -> Unit)?) {
    f?.invoke()
}

suspend fun passNone(f: (() -> String)?) = none(f)
suspend fun passTwo(f: ((Int, String) -> String)?) = two(f)
suspend fun passExtension(f: (Int.() -> String)?) = extension(f)
suspend fun passEffect(f: (() -> Unit)?) = effect(f)

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
    (block as (Continuation<Any?>) -> Any?)(NeverResumed)

fun box(): String {
    val receiver: Int.() -> String = { "r$this" }
    var effects = 0
    val result = run {
        passNone { "x" } + passTwo { i, s -> "$i$s" } + passExtension(receiver) +
            passEffect { effects++ }.let { "u" }
    }
    if (result != "x1ar2u") return "fail: $result"
    if (effects != 1) return "fail: $effects effects"
    return "OK"
}
