// MODULE: lib
// FILE: Registry.kt
package fixture

// Library classes whose only primary constructor is private, as the Wasm standard library's
// `HashMap` takes its storage in a library-private primary constructor. A consumer target lowers
// each construction from the library's bodies: the public constructors delegate to the private one,
// which no provider publishes to source.
class Registry<K, V> private constructor(private var key: K?, private var value: V?, private val tag: Int) {
    constructor() : this(null, null, 1)

    constructor(key: K, value: V) : this() {
        put(key, value)
    }

    fun put(key: K, value: V) {
        this.key = key
        this.value = value
    }

    fun get(key: K): V? = if (this.key == key) value else null
}

class Marker private constructor(val label: String) {
    constructor() : this("marker")
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val empty = Registry<String, Int>()
    empty.put("a", 1)
    if (empty.get("a") != 1) return "get a"
    if (empty.get("b") != null) return "get b"
    val seeded = Registry("k", "v")
    if (seeded.get("k") != "v") return "seeded"
    if (Marker().label != "marker") return "marker"
    return "OK"
}
