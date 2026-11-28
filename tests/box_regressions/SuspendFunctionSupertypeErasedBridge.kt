import kotlin.coroutines.*

class Probe : suspend (Int) -> String {
    override suspend fun invoke(p1: Int): String = if (p1 == 7) "OK" else "fail"
}

object Done : Continuation<String> {
    override val context: CoroutineContext get() = throw UnsupportedOperationException()
    override fun resumeWith(result: Result<String>) {}
}

fun box(): String {
    val erased: Any = Probe()
    @Suppress("UNCHECKED_CAST")
    val physical = erased as (Int, Continuation<String>) -> Any?
    return physical(7, Done) as String
}
