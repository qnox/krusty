import kotlin.coroutines.*
import kotlin.properties.Delegates

class Name(val name: String) : AbstractCoroutineContextElement(Name) {
    companion object Key : CoroutineContext.Key<Name>
}

private fun read(value: Any): Any = value

fun box(): String {
    val delegates1 = read(Delegates)
    val delegates2 = read(Delegates)
    val result1 = read(Result.Companion)
    val result2 = read(Result.Companion)
    if (delegates1 !== delegates2 || result1 !== result2 || delegates1 === result1) {
        return "runtimeObjectIdentity"
    }

    var delegated by Delegates.notNull<String>()
    delegated = "delegated"
    if (delegated != "delegated") return "delegatesNotNull"
    val delegatedObject = Delegates.notNull<String>()
    val delegatedText = delegatedObject.toString()
    if (delegatedText != "NotNullProperty(value not initialized yet)") return "delegatesToString"
    if (runCatching { "runCatching" }.getOrThrow() != "runCatching") return "runCatching"

    val a: Any = EmptyCoroutineContext
    val b: Any = EmptyCoroutineContext
    if (a !== b) return "identity"
    val element = Name("x")
    val folded = EmptyCoroutineContext.fold(0) { n, _ -> n + 1 }
    if (folded != 0) return "fold"
    val back = EmptyCoroutineContext + element
    if (back !== element) return "emptyPlus"
    return "OK"
}
