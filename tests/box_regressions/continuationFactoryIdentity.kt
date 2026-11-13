// The stdlib's `Continuation(context) { ... }` builds an anonymous `Continuation` object: an
// ordinary object with a stable identity of its own.
import kotlin.coroutines.*

object Context : CoroutineContext {
    override fun <R> fold(initial: R, operation: (R, CoroutineContext.Element) -> R): R = initial
    override fun <E : CoroutineContext.Element> get(key: CoroutineContext.Key<E>): E? = null
    override fun minusKey(key: CoroutineContext.Key<*>): CoroutineContext = this
}

fun box(): String {
    val continuation = Continuation<Int>(Context) {}
    if (continuation != continuation) return "FAIL equals"
    val other = Continuation<Int>(Context) {}
    if (other == continuation) return "FAIL distinct"
    return "OK"
}
