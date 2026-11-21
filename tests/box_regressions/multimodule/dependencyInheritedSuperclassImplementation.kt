// MODULE: lib
// FILE: Cursors.kt
package fixture

// A subclass implements an interface member with the body it inherits from its superclass, which
// does not declare the interface and which no body calls: `Cursor.remove` implements
// `Removable.remove` for each subclass, as the Wasm standard library's `HashMap.Itr.remove`
// implements `MutableIterator.remove` for its key, value and entry iterators.
interface Removable {
    fun remove()
    fun next(): Int
}

open class Cursor(protected val items: IntArray) {
    protected var index = 0
    var removed = 0

    fun hasNext(): Boolean = index < items.size

    fun remove() {
        removed += 1
    }
}

class Doubling(items: IntArray) : Cursor(items), Removable {
    override fun next(): Int {
        index += 1
        return items[index - 1] * 2
    }
}

class Plain(items: IntArray) : Cursor(items), Removable {
    override fun next(): Int {
        index += 1
        return items[index - 1]
    }
}

fun drain(cursor: Removable): Int {
    val value = cursor.next()
    cursor.remove()
    return value
}

fun doubling(items: IntArray): Doubling = Doubling(items)

fun plain(items: IntArray): Plain = Plain(items)

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val first = doubling(intArrayOf(1, 2, 3))
    if (drain(first) != 2) return "doubling"
    if (first.removed != 1 || !first.hasNext()) return "doubling removed ${first.removed}"
    val second = plain(intArrayOf(5, 6))
    if (drain(second) != 5) return "plain"
    if (second.removed != 1) return "plain removed ${second.removed}"
    return "OK"
}
