// A library object: its instance and its members come from the library.
import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.EmptyCoroutineContext

fun box(): String {
    val folded = EmptyCoroutineContext.fold("O") { acc, _ -> acc + "x" }
    if (folded != "O") return "Fail fold: $folded"
    val context: CoroutineContext = EmptyCoroutineContext
    if (EmptyCoroutineContext.plus(context) !== context) return "Fail plus"
    if (EmptyCoroutineContext.plus(EmptyCoroutineContext) !== EmptyCoroutineContext) {
        return "Fail plus itself"
    }
    return folded + "K"
}
