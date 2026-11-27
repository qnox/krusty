interface Ordered : Comparable<String> {
    override fun compareTo(other: String): Int
}

class OrderedImpl(private val own: String) : Ordered {
    override fun compareTo(other: String): Int = own.compareTo(other)
}

class Delegating(ordered: Ordered) : Ordered by ordered

fun box(): String {
    val d = Delegating(OrderedImpl("b"))
    if (d.compareTo("a") <= 0) return "after: ${d.compareTo("a")}"
    if (d.compareTo("c") >= 0) return "before: ${d.compareTo("c")}"
    if (d.compareTo("b") != 0) return "same: ${d.compareTo("b")}"
    return "OK"
}
