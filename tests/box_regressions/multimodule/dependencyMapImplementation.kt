// A library class implementing Map, read through Map-typed receivers in a module that implements
// no Map of its own. A subclass inherits the members it does not override.

// MODULE: library
// FILE: library.kt
package maps

/** The squares of `0 until limit`, by their roots. The views are not asked for here. */
open class Squares(private val limit: Int) : Map<Int, Int> {
    override val size: Int get() = limit
    override fun isEmpty(): Boolean = limit == 0
    override fun containsKey(key: Int): Boolean = key >= 0 && key < limit
    override fun containsValue(value: Int): Boolean {
        for (root in 0 until limit) {
            if (root * root == value) return true
        }
        return false
    }
    override fun get(key: Int): Int? = if (containsKey(key)) key * key else null
    override val keys: Set<Int> get() = throw UnsupportedOperationException("keys")
    override val values: Collection<Int> get() = throw UnsupportedOperationException("values")
    override val entries: Set<Map.Entry<Int, Int>>
        get() = throw UnsupportedOperationException("entries")
}

class Fixed(limit: Int) : Squares(limit) {
    override fun get(key: Int): Int? = -1
}

fun squares(limit: Int): Map<Int, Int> = Squares(limit)
fun fixed(limit: Int): Map<Int, Int> = Fixed(limit)

// MODULE: main(library)
// FILE: main.kt
import maps.fixed
import maps.squares

fun box(): String {
    val map = squares(3)
    if (map.size != 3) return "size ${map.size}"
    if (map.isEmpty()) return "isEmpty"
    if (!map.containsKey(2) || map.containsKey(3)) return "containsKey"
    if (!map.containsValue(4) || map.containsValue(3)) return "containsValue"
    if (map[2] != 4 || map[5] != null) return "get"
    if (!squares(0).isEmpty()) return "empty"
    val fixed = fixed(2)
    if (fixed.isEmpty() || fixed.size != 2 || !fixed.containsKey(1)) return "inherited"
    if (fixed[7] != -1) return "overridden get"
    return "OK"
}
