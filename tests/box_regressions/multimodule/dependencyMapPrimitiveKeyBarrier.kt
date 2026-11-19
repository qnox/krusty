// A library Map<Int, Int> class read through a Map<Any?, Any?> receiver in a module that implements
// no Map of its own. The override takes the primitive, so the special bridge's barrier must check
// the boxed argument: a key or value that is not an Int, and a null, answer the neutral result
// instead of being unboxed into the override.

// MODULE: library
// FILE: library.kt
package maps

class Numbers : Map<Int, Int> {
    override val size: Int get() = 1
    override fun isEmpty(): Boolean = false
    override fun containsKey(key: Int): Boolean = key == 7
    override fun containsValue(value: Int): Boolean = value == 49
    override fun get(key: Int): Int? = if (key == 7) 49 else null
    override val keys: Set<Int> get() = throw UnsupportedOperationException("keys")
    override val values: Collection<Int> get() = throw UnsupportedOperationException("values")
    override val entries: Set<Map.Entry<Int, Int>>
        get() = throw UnsupportedOperationException("entries")
}

fun numbers(): Map<Int, Int> = Numbers()

// MODULE: main(library)
// FILE: main.kt
import maps.numbers

@Suppress("UNCHECKED_CAST")
fun box(): String {
    val wide = numbers() as Map<Any?, Any?>
    if (!wide.containsKey(7)) return "containsKey"
    if (!wide.containsValue(49)) return "containsValue"
    if (wide[7] != 49) return "get"
    if (wide.containsKey("7")) return "wrong-type containsKey"
    if (wide.containsValue("49")) return "wrong-type containsValue"
    if (wide["7"] != null) return "wrong-type get"
    if (wide.containsKey(7L)) return "long containsKey"
    if (wide.containsValue(49L)) return "long containsValue"
    if (wide[7L] != null) return "long get"
    if (wide.containsKey(null)) return "null containsKey"
    if (wide.containsValue(null)) return "null containsValue"
    if (wide[null] != null) return "null get"
    return "OK"
}
