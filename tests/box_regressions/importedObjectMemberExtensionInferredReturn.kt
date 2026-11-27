import Ref.asRef
import Ref.wrapped

class Resolved(val value: String)

class Wrapped<T>(val value: T)

object Ref {
    fun String.asRef() = Resolved(this)

    fun <T> T.wrapped() = Wrapped(this)

    fun wrapInside(s: String) = s.wrapped()
}

fun use(s: String) = s.asRef()

fun wrap(s: String) = s.wrapped()

fun box(): String {
    val wrapped: Wrapped<String> = wrap("O")
    if (wrapped.value.length != 1) return "fail wrapped"
    if (Ref.wrapInside("K").value.length != 1) return "fail inside"
    return wrapped.value + Ref.wrapInside(use("K").value).value
}
