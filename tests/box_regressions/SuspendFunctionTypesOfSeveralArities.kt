// WITH_STDLIB
import kotlin.coroutines.*

// One object implements suspend function types of two arities, and each is called through its own
// type from a coroutine: the continuation counts as an operand, so the two must not share a body.
class SuspendPair : suspend (Int) -> String, suspend (Int, Int) -> String {
    override suspend fun invoke(a: Int): String = "one:$a"
    override suspend fun invoke(a: Int, b: Int): String = "two:${a + b}"
}

suspend fun callOne(f: suspend (Int) -> String) = f(7)
suspend fun callTwo(f: suspend (Int, Int) -> String) = f(3, 4)

fun box(): String {
    val pair = SuspendPair()
    var result = "FAIL: not run"
    val body: suspend () -> Unit = {
        val one = callOne(pair)
        val two = callTwo(pair)
        result = if (one == "one:7" && two == "two:7") "OK" else "FAIL: $one $two"
    }
    body.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
    return result
}
