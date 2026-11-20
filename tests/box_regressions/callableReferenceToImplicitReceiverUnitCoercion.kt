class Items<E> {
    var joined = ""
    var count = 0
    fun add(item: E): Boolean { joined += item; count++; return true }
}
fun <E> build(body: Items<E>.() -> Unit): Items<E> { val items = Items<E>(); items.body(); return items }
class Source<T>(val first: T, val second: T)
fun <T> Source<T>.each(action: (T) -> Unit) { action(first); action(second) }

fun box(): String {
    val values = Source("O", "K")
    val built = build { values.each(::add) }
    if (built.count != 2) return "fail: built ${built.count}"
    val receiver = Items<String>()
    with(receiver) { values.each(::add) }
    if (receiver.joined != "OK") return "fail: with ${receiver.joined}"
    return built.joined
}
