// Every suspend function value of `suspendLambdaValues.kt` invoked, and the suspend function
// beside them called: none of them suspends, so each call completes in place and returns its
// value without resuming the continuation it is handed. A suspend function value is invoked as
// the function value of one more argument, the caller's continuation, as kotlinc's
// `SuspendFunction0` is a `Function1`.
import kotlin.coroutines.*

suspend fun completesInPlace(x: Int): Int = x + 1

// A context with no elements.
object NoElements : CoroutineContext {
    override fun <E : CoroutineContext.Element> get(key: CoroutineContext.Key<E>): E? = null
    override fun <R> fold(initial: R, operation: (R, CoroutineContext.Element) -> R): R = initial
    override fun minusKey(key: CoroutineContext.Key<*>): CoroutineContext = this
}

// The continuation every call is handed. A call that completes in place never resumes it.
object NeverResumed : Continuation<Any?> {
    override val context: CoroutineContext get() = NoElements
    override fun resumeWith(result: Result<Any?>) {
        throw IllegalStateException("a call that completes in place resumed its continuation")
    }
}

@Suppress("UNCHECKED_CAST")
fun invoke0(f: suspend () -> Any?): Any? = (f as (Continuation<Any?>) -> Any?)(NeverResumed)

@Suppress("UNCHECKED_CAST")
fun invoke1(f: suspend (Int) -> Any?, x: Int): Any? =
    (f as (Int, Continuation<Any?>) -> Any?)(x, NeverResumed)

@Suppress("UNCHECKED_CAST")
fun invokeOn(f: suspend Int.() -> Any?, x: Int): Any? =
    (f as (Int, Continuation<Any?>) -> Any?)(x, NeverResumed)

fun box(): String {
    val captured = "K"
    val n: suspend (Int) -> Int = { it }
    val s: suspend () -> String = { "O" + captured }
    val e: suspend Int.() -> Int = { this + 1 }
    var ran = false
    val u: suspend () -> Unit = { ran = true }
    if (invoke1(n, 41) != 41) return "parameter"
    if (invokeOn(e, 41) != 42) return "receiver"
    if (invoke0(u) != Unit || !ran) return "unit"
    if (invoke1({ completesInPlace(it) }, 1) != 2) return "leaf"
    return if (invoke0(s) == "OK") "OK" else "capture"
}
