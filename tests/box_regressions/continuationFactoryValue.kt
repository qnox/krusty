// WITH_STDLIB

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext

fun box(): String {
    val value: Any? = Continuation<Unit>(EmptyCoroutineContext) {}
    return if (value != null) "OK" else "FAIL"
}
