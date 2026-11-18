// A class inherits `CoroutineContext.plus`, a default member of a library interface, and it runs on
// the class's instances. It calls the interface's members virtually, so the contexts the class
// implements take part, and it reads the library object `EmptyCoroutineContext`.
import kotlin.coroutines.*

class Labels(val text: String) : CoroutineContext {
    var folded = 0

    override fun <E : CoroutineContext.Element> get(key: CoroutineContext.Key<E>): E? = null

    override fun <R> fold(initial: R, operation: (R, CoroutineContext.Element) -> R): R {
        folded++
        return initial
    }

    override fun minusKey(key: CoroutineContext.Key<*>): CoroutineContext = this
}

fun box(): String {
    val left = Labels("left")
    // The empty context is returned as it is, with no fold.
    if (left + EmptyCoroutineContext !== left) return "fail: empty context"
    if (left.folded != 0) return "fail: folded the empty context"
    // Any other context folds over this one, and a context with no elements leaves it as it is.
    val right = Labels("right")
    if (left + right !== left) return "fail: context with no elements"
    if (right.folded != 1 || left.folded != 0) return "fail: ${left.folded}/${right.folded} folds"
    return "OK"
}
