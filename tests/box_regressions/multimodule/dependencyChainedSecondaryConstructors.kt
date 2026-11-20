// MODULE: lib
// FILE: Table.kt
package fixture

// Library classes whose secondary constructors reach the primary one through another secondary
// constructor, as the Wasm standard library's `HashMap()` delegates to `HashMap(initialCapacity)`.
// A consumer target lowers each construction from the library's bodies: every constructor on the
// chain runs its own body after the one it delegates to.
class Counter(start: Int) {
    var value: Int = start
    var steps = 0

    constructor(label: String) : this(label.length) {
        steps += 1
    }

    constructor() : this("seven!!") {
        steps += 10
    }
}

class Table<K, V>(val capacity: Int) {
    private var key: K? = null
    private var value: V? = null

    constructor(capacity: Int, spare: Int) : this(capacity + spare)

    constructor() : this(2, 2)

    fun put(key: K, value: V) {
        this.key = key
        this.value = value
    }

    fun get(key: K): V? = if (this.key == key) value else null
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val counter = Counter()
    if (counter.value != 7) return "counter value ${counter.value}"
    if (counter.steps != 11) return "counter steps ${counter.steps}"
    val labelled = Counter("abc")
    if (labelled.value != 3 || labelled.steps != 1) return "labelled"
    val table = Table<String, Int>()
    if (table.capacity != 4) return "capacity ${table.capacity}"
    table.put("b", 2)
    if (table.get("b") != 2) return "get b"
    if (table.get("c") != null) return "get c"
    return "OK"
}
