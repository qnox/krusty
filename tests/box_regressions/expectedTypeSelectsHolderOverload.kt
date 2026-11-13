class Holder<T>(val value: T)
class P<T>(val tag: String)

fun <T> p(value: T): P<T> = P("plain")
fun <T> p(holder: Holder<T>): P<T> = P("holder")

class Q<T>(val tag: String) {
    constructor(value: T, marker: Int) : this("plain")
    constructor(holder: Holder<T>, marker: Int) : this("holder")
}

fun box(): String {
    val x: P<Any> = p(Holder(1))
    if (x.tag != "holder") return "function: ${x.tag}"
    val y: Q<Any> = Q(Holder(1), 0)
    if (y.tag != "holder") return "constructor: ${y.tag}"
    return "OK"
}
