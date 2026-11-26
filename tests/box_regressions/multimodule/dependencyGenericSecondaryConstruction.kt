// MODULE: lib
// FILE: Bags.kt
package fixture

// A library body constructs a generic class through a secondary constructor, at its own type
// parameter: the constructor it runs is the one the call selected, not one whose parameter list
// matches the call's instantiated `Collection<T>`.
class Bag<E>(val size: Int) {
    constructor(items: Collection<E>) : this(items.size + 100)
}

fun <T> bagOf(items: Collection<T>): Bag<T> = Bag(items)

fun emptyBag(): Bag<String> = Bag(3)

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (bagOf(listOf("a", "b")).size != 102) return "bagOf ${bagOf(listOf("a", "b")).size}"
    if (emptyBag().size != 3) return "emptyBag ${emptyBag().size}"
    return "OK"
}
