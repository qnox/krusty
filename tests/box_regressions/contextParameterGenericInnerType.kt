// A context parameter of an unqualified inner type inside a generic outer: the outer's type
// arguments come from the dispatch receiver, not from the context parameter being resolved.
class Holder<T>(val value: T) {
    inner class Item {
        fun get(): T = value
    }

    context(item: Item)
    fun read(): T = item.get()
}

fun box(): String {
    val holder = Holder("OK")
    return with(holder.Item()) { holder.read() }
}
