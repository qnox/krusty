// MODULE: lib
// FILE: Slots.kt
package fixture

// Generic library classes a consumer target lowers from the library's bodies, as the standard
// library's `ArrayList<E>`: a generic final class, a generic subclass delegating to a generic
// open base with its own type parameter, also after an interface among its supertypes, and a
// secondary constructor delegating to the primary one. The consumer calls final members; a final
// member reaches the base's members.
class Cell<T>(first: T) {
    private var item: T = first
    private var writes = 0

    fun get(): T = item

    fun put(value: T): T {
        val old = item
        item = value
        writes += 1
        return old
    }

    val written: Int
        get() = writes
}

open class Slot<T>(first: T) {
    private var item: T = first
    private var writes = 0

    fun get(): T = item

    fun put(value: T): T {
        val old = item
        item = value
        writes += 1
        return old
    }

    val written: Int
        get() = writes
}

class Pair2<E>(left: E, private val right: E) : Slot<E>(left) {
    constructor(both: E) : this(both, both)

    fun other(): E = right

    fun first(): E = get()
}

interface Named {
    fun name(): String
}

// Its superclass follows an interface among its supertypes, as `ArrayList`'s does.
class Labeled<E>(item: E) : Named, Slot<E>(item) {
    override fun name(): String = "labeled"

    fun item(): E = get()
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val slot = Cell("O")
    if (slot.put("K") != "O") return "put"
    if (slot.get() != "K") return "get"
    if (slot.written != 1) return "written"
    val pair = Pair2("A", "B")
    if (pair.first() != "A" || pair.other() != "B") return "pair"
    val same = Pair2(7)
    if (same.first() + same.other() != 14) return "same"
    val labeled = Labeled(3)
    if (labeled.item() != 3 || labeled.name() != "labeled") return "labeled"
    return "OK"
}
