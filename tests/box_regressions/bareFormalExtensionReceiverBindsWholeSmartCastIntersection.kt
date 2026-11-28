// A bare formal extension receiver (`<T> T.also`, `T.let`) binds the whole smart-cast
// intersection, nullability included, so the lambda's value is still both constituents.
interface Msg {
    fun text(): String
}

interface Holder<out T> {
    fun held(): String = "held"
}

class Both(val s: String) : Msg, Holder<Int> {
    override fun text() = s
}

class Text(val s: String) : Msg {
    override fun text() = s
}

fun render(element: Msg): String =
    if (element is Holder<*>) {
        element.also { it.text(); it.held() }.let { it.text() + ":" + it.held() }
    } else {
        "plain"
    }

fun renderNullable(element: Msg?): String =
    if (element is Holder<*>?) {
        element.let { it?.text() + ":" + it?.held() }
    } else {
        "plain"
    }

fun box(): String {
    val both = render(Both("b"))
    val plain = render(Text("t"))
    val nullable = renderNullable(Both("n"))
    val absent = renderNullable(null)
    return if (both == "b:held" && plain == "plain" && nullable == "n:held" && absent == "null:null") "OK"
    else "FAIL: $both $plain $nullable $absent"
}
