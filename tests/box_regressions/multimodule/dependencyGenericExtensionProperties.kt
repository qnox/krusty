// MODULE: lib
// FILE: Slots.kt
package fixture

// A generic extension property declares its own type parameter, which each access binds: the
// library body reads it at the receiver's element type, as a call of a generic function does.
val <T> List<T>.lastSlot: Int
    get() = size - 1

fun lastOf(items: List<String>): String = items[items.lastSlot]

fun lastSlotOf(numbers: List<Int>): Int = numbers.lastSlot

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (lastOf(listOf("a", "b", "c")) != "c") return "lastOf ${lastOf(listOf("a", "b", "c"))}"
    if (lastSlotOf(listOf(4, 5)) != 1) return "lastSlotOf ${lastSlotOf(listOf(4, 5))}"
    return "OK"
}
