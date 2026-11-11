// WITH_STDLIB

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext

fun box(): String {
    val c = Continuation<Unit>(EmptyCoroutineContext) {}
    val member = (c as Any) is Continuation<*>
    val context = c.context === EmptyCoroutineContext
    return if (member && context) "OK" else "FAIL"
}
