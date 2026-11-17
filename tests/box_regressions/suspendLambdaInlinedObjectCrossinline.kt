// KOTLINC_VERSION: [2.5.0,)
// A suspend lambda calls an inline function whose anonymous object captures the call's
// `crossinline` lambda argument. The argument is spliced into the object's members, so the copy
// of the object made for the suspend lambda carries only the spliced body and no callable
// implementation of the argument. Both a plain and a suspend member of the object read it. None
// of the calls suspends, so each completes in place without resuming its continuation.
// kotlinc before 2.5.0 miscompiles the suspend member's copy (KT-86095), so it has no oracle there.
import kotlin.coroutines.*

abstract class Runner {
    abstract fun run(): String
    internal abstract suspend fun runSuspending(): String
}

suspend fun completesInPlace() {}

inline fun makeRunner(crossinline f: () -> String) =
    object : Runner() {
        override fun run(): String = f()
        override suspend fun runSuspending(): String {
            completesInPlace()
            return f()
        }
    }

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

fun box(): String {
    val captured = "K"
    val plain = invoke0 { makeRunner { "O" + captured }.run() }
    if (plain != "OK") return "plain: $plain"
    val suspending = invoke0 { makeRunner { "O" + captured }.runSuspending() }
    if (suspending != "OK") return "suspending: $suspending"
    return "OK"
}
