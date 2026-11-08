// A suspend method called through an open base runs the override its receiver selects, not the
// function in the base's slot. Both bodies complete in place, but the base slot alone does not
// prove that: an override in another module could suspend.
import kotlin.coroutines.*

open class Base {
    open suspend fun value(): String = "base"
}

class Derived : Base() {
    override suspend fun value(): String = "OK"
}

suspend fun read(base: Base): String = base.value()

// A context with no elements.
object NoElements : CoroutineContext {
    override fun <E : CoroutineContext.Element> get(key: CoroutineContext.Key<E>): E? = null
    override fun <R> fold(initial: R, operation: (R, CoroutineContext.Element) -> R): R = initial
    override fun minusKey(key: CoroutineContext.Key<*>): CoroutineContext = this
}

// The continuation the call is handed. A call that completes in place never resumes it.
object NeverResumed : Continuation<Any?> {
    override val context: CoroutineContext get() = NoElements
    override fun resumeWith(result: Result<Any?>) {
        throw IllegalStateException("a call that completes in place resumed its continuation")
    }
}

@Suppress("UNCHECKED_CAST")
fun invoke0(f: suspend () -> Any?): Any? = (f as (Continuation<Any?>) -> Any?)(NeverResumed)

fun box(): String {
    val viaBase = invoke0 { read(Derived()) }
    if (viaBase != "OK") return "override: $viaBase"
    return if (invoke0 { read(Base()) } == "base") "OK" else "base"
}
