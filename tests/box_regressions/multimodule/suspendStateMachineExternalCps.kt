// MODULE: library
// FILE: dependency.kt
package dependency

import kotlin.coroutines.Continuation
import kotlin.coroutines.resume
import kotlin.coroutines.suspendCoroutine

private var pendingContinuation: Continuation<Any?>? = null
private var pendingValue: Any? = null

@Suppress("UNCHECKED_CAST")
suspend fun <T> dependencyIdentity(value: T): T = suspendCoroutine { continuation ->
    pendingValue = value
    pendingContinuation = continuation as Continuation<Any?>
}

fun resumeDependency() {
    val continuation = pendingContinuation ?: error("missing dependency continuation")
    val value = pendingValue
    pendingContinuation = null
    pendingValue = null
    continuation.resume(value)
}

// MODULE: main(library)
// FILE: main.kt
import dependency.dependencyIdentity
import dependency.resumeDependency
import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

private suspend fun caller(): String = dependencyIdentity("OK")

fun box(): String {
    var result: Result<String>? = null
    ::caller.startCoroutine(Continuation(EmptyCoroutineContext) { result = it })
    if (result != null) return "completed before dependency resume"
    resumeDependency()
    return result?.getOrThrow() ?: "did not complete"
}
