// MODULE: lib
// A library interface declares properties of a type parameter, and the library reads and writes
// them through the interface.
interface Holder<T> {
    val item: T
    var slot: T
}

fun <T> exchange(holder: Holder<T>, replacement: T): T {
    val old = holder.slot
    holder.slot = replacement
    return old
}

fun total(holder: Holder<Int>): Int = holder.item + holder.slot

// MODULE: main(lib)
// A class of the consumer implements them with the type of its own: `Int` for `T`, a carrier other
// than the interface's, so the library reaches each accessor through a bridge.
class IntHolder(private val fixed: Int, start: Int) : Holder<Int> {
    override val item: Int
        get() = fixed
    override var slot: Int = start
}

fun box(): String {
    val holder = IntHolder(10, 1)
    if (total(holder) != 11) return "total: ${total(holder)}"
    val old = exchange(holder, 5)
    if (old != 1) return "old: $old"
    if (holder.slot != 5) return "own slot: ${holder.slot}"
    if (total(holder) != 15) return "total after: ${total(holder)}"
    return "OK"
}
