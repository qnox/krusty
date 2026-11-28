// A bare type-parameter receiver (`<T> T.also`, `<T> T.twice`) on a value a smart cast proved to be
// two incomparable types binds `T` to the whole intersection, so both constituents' members stay
// reachable through it, at the receiver's own nullability.
interface Msg {
    fun text(): String
}

interface Holder<out T> {
    fun held(): String = "held"
}

class Both(val s: String) : Msg, Holder<Int> {
    override fun text() = s
}

fun <T> T.twice(): Pair<T, T> = this to this

fun render(element: Msg): String = when (element) {
    is Holder<*> -> {
        val also = element.also { it.text(); it.held() }
        val pair = element.twice()
        pair.first.text() + pair.second.held() + also.text()
    }
    else -> "plain"
}

fun nullable(element: Msg?): String {
    if (element !is Holder<*>?) return "no"
    val also = element?.also { it.text(); it.held() }
    return also?.text() ?: "null"
}

fun box(): String {
    val result = render(Both("b")) + nullable(Both("n")) + nullable(null)
    return if (result == "bheldbnnull") "OK" else "Fail: $result"
}
