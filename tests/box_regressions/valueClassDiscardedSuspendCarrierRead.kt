// A carrier read out of a suspend call and then discarded still casts the call's Object result to
// the carrier: the suspend call already hands the carrier over, so there is no unbox to drop.
import kotlin.coroutines.*

OPTIONAL_JVM_INLINE_ANNOTATION
value class IC(val s: String)

interface I {
    suspend fun foo(s: String): IC
}

class D : I {
    override suspend fun foo(s: String): IC = IC(s)
}

class C(val d: I) : I by d

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(object : Continuation<Unit> {
        override val context: CoroutineContext = EmptyCoroutineContext
        override fun resumeWith(r: Result<Unit>) {
            r.getOrThrow()
        }
    })
}

fun box(): String {
    var ran = false
    builder {
        ran = true
        C(D()).foo("IGNORE ME").s
    }
    return if (ran) "OK" else "FAIL"
}
