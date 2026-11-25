// `List<String>::isEmpty` and `ArrayList<String>::isEmpty` name the member function, not a property
// derived from a getter of the same name, so the reference also converts to a `suspend` function type.
import kotlin.coroutines.*

fun plainListEmpty(): (List<String>) -> Boolean = List<String>::isEmpty

fun listEmpty(): suspend (List<String>) -> Boolean = List<String>::isEmpty

fun arrayListEmpty(): suspend (ArrayList<String>) -> Boolean = ArrayList<String>::isEmpty

fun box(): String {
    var result = "${plainListEmpty()(emptyList())}${plainListEmpty()(listOf("a"))}"
    suspend {
        result += listEmpty()(emptyList())
        result += listEmpty()(listOf("a"))
        result += arrayListEmpty()(arrayListOf())
        result += arrayListEmpty()(arrayListOf("a"))
    }.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
    return if (result == "truefalsetruefalsetruefalse") "OK" else "fail: $result"
}
