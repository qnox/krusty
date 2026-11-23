// MODULE: lib
// FILE: Slot.kt
package fixture

// A library body asserts a value of a nullable-bounded type parameter non-null (`item!!`): the
// assertion's type is the definitely-non-null `T & Any`, as the Wasm standard library's
// `HashMap` entries read their keys.
class Slot<T>(private var item: T?) {
    fun take(): T {
        val value = item!!
        item = null
        return value
    }

    fun has(): Boolean = item != null
}

class Pair2<K, V>(private val key: K?, private val value: V?) {
    fun describe(): String = "${key!!}=${value!!}"
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val slot = Slot<String>("O")
    if (!slot.has()) return "has"
    val first = slot.take()
    if (slot.has()) return "taken"
    val second = Slot<String?>("K").take()
    if (second != "K") return "second $second"
    if (Pair2("a", 1).describe() != "a=1") return "describe"
    return first + second
}
