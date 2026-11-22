// A class implementing `Collection` must not turn a `List` receiver's members into calls through
// an interface table the runtime's lists do not carry.
class Holder(val items: List<String>) : Collection<String> {
    override val size: Int get() = items.size
    override fun contains(element: String): Boolean = items.contains(element)
    override fun containsAll(elements: Collection<String>): Boolean = items.containsAll(elements)
    override fun isEmpty(): Boolean = items.isEmpty()
    override fun iterator(): Iterator<String> = items.iterator()
}

fun box(): String {
    val h = Holder(listOf("a", "b"))
    if (!h.contains("a") || h.contains("z")) return "fail contains"
    if (!h.containsAll(listOf("a", "b"))) return "fail containsAll"
    if (h.isEmpty() || !Holder(emptyList()).isEmpty()) return "fail isEmpty"
    val list = mutableListOf("a", "b", "c")
    if (!list.containsAll(listOf("a"))) return "fail plain"
    if (!list.containsAll(Holder(listOf("a", "b")))) return "fail holder"
    return "OK"
}
