interface Msg {
    fun text(): String
}

interface Holder<out T>

fun Msg.toChain(): String = "msg:" + text()
fun Holder<Msg>.toChain(): String = "holder"

class Text(val s: String) : Msg {
    override fun text() = s
}

class Both(val s: String) : Msg, Holder<Int> {
    override fun text() = s
}

fun render(element: Msg): String = when (element) {
    is Holder<*> -> element.toChain()
    else -> "plain"
}

fun box(): String {
    val both = render(Both("b"))
    return if (both == "msg:b" && render(Text("t")) == "plain") "OK" else "FAIL: $both"
}
