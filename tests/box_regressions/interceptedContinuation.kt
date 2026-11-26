// Starting a coroutine intercepts its first continuation through the context's interceptor;
// `intercepted()` of a continuation that is not a coroutine is the continuation itself.
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

var log = ""

object Interceptor : ContinuationInterceptor {
    override val key: CoroutineContext.Key<*> get() = ContinuationInterceptor

    @Suppress("UNCHECKED_CAST")
    override fun <E : CoroutineContext.Element> get(key: CoroutineContext.Key<E>): E? =
        if (key === ContinuationInterceptor) this as E else null

    override fun minusKey(key: CoroutineContext.Key<*>): CoroutineContext =
        if (key === ContinuationInterceptor) EmptyCoroutineContext else this

    override fun <R> fold(initial: R, operation: (R, CoroutineContext.Element) -> R): R =
        operation(initial, this)

    override fun plus(context: CoroutineContext): CoroutineContext =
        if (context === EmptyCoroutineContext) this else context

    override fun releaseInterceptedContinuation(continuation: Continuation<*>) {}

    override fun <T> interceptContinuation(continuation: Continuation<T>): Continuation<T> {
        log += "I"
        return continuation
    }
}

fun start(context: CoroutineContext, block: suspend () -> String): String {
    var result = ""
    block.startCoroutine(Continuation(context) { result = it.getOrThrow() })
    return result
}

fun box(): String {
    val intercepted = start(Interceptor) { log += "B"; "O" }
    if (log != "IB") return "fail intercepted: $log"
    log = ""
    val plain = start(EmptyCoroutineContext) { log += "P"; "K" }
    if (log != "P") return "fail plain: $log"
    val completion = Continuation<Unit>(Interceptor) {}
    if (completion.intercepted() !== completion) return "fail completion"
    if (log != "P") return "fail completion log: $log"
    return intercepted + plain
}
