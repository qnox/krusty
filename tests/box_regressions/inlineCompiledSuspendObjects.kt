import kotlin.coroutines.*

interface Named {
    suspend fun name(): String
}

var pending: Any? = null

inline fun <reified T> make(): Named = object : Named {
    override suspend fun name(): String {
        suspendCoroutine<Unit> { pending = it }
        return T::class.simpleName!!
    }
}

fun box(): String {
    var result = ""
    suspend { make<String>().name() }.startCoroutine(Continuation(EmptyCoroutineContext) {
        result = it.getOrThrow()
    })
    @Suppress("UNCHECKED_CAST")
    (pending as Continuation<Unit>).resume(Unit)
    return if (result == "String") "OK" else result
}
