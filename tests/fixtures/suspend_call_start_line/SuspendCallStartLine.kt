// kotlinc marks a suspend callee's call line before its receiver and arguments, so a call whose
// receiver starts on an earlier line opens on the call's line and then on the receiver's.

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

class Box(val v: String) {
    suspend fun put(x: String): String = v + x

    suspend fun touch() {}
}

suspend fun <T> T.await(): T = this

suspend fun step(s: String): Int = s.length

suspend fun unitStep(s: String) {}

fun box(s: String) = Box(s)

val log = StringBuilder()

suspend fun shapes(x: String): String {
    box(x)
        .put(x)
    box(x)
        .touch()
    x
        .await()
    log.append(
        step(
            x,
        ),
    )
    unitStep(
        x,
    )
    val n = box(x)
        .put("y")
        .length
    if (n > 0) {
        box(x)
            .touch()
    }
    return box(x)
        .put("z")
}

suspend fun tail(x: String) =
    box(x)
        .put("t")

class Collect : Continuation<String> {
    var result = ""
    override val context get() = EmptyCoroutineContext

    override fun resumeWith(result: Result<String>) {
        this.result = result.getOrThrow()
    }
}

fun box(): String {
    val collect = Collect()
    suspend { shapes("a") + tail("b") }.startCoroutine(collect)
    val result = collect.result
    return if (result == "azbt" && log.toString() == "1") "OK" else "fail: $result $log"
}
