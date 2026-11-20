// A library Map class read through a Map<Any?, Any?> receiver in a module that implements no Map
// of its own. The overrides accept every String, so only the special bridge's barrier can tell a
// key or value that is not one: it answers the neutral result (false, null) instead of reaching
// the String-specialized override. A null the override's type admits still reaches it.

// MODULE: library
// FILE: library.kt
package maps

class Words : Map<String, String> {
    override val size: Int get() = 2
    override fun isEmpty(): Boolean = false
    override fun containsKey(key: String): Boolean = true
    override fun containsValue(value: String): Boolean = true
    override fun get(key: String): String? = "found"
    override val keys: Set<String> get() = throw UnsupportedOperationException("keys")
    override val values: Collection<String>
        get() = throw UnsupportedOperationException("values")
    override val entries: Set<Map.Entry<String, String>>
        get() = throw UnsupportedOperationException("entries")
}

class Optional : Map<String?, String?> {
    override val size: Int get() = 1
    override fun isEmpty(): Boolean = false
    override fun containsKey(key: String?): Boolean = true
    override fun containsValue(value: String?): Boolean = true
    override fun get(key: String?): String? = "found"
    override val keys: Set<String?> get() = throw UnsupportedOperationException("keys")
    override val values: Collection<String?>
        get() = throw UnsupportedOperationException("values")
    override val entries: Set<Map.Entry<String?, String?>>
        get() = throw UnsupportedOperationException("entries")
}

// A key type that is a type parameter admits null whatever it is later instantiated with.
class Generic<K> : Map<K, String> {
    override val size: Int get() = 1
    override fun isEmpty(): Boolean = false
    override fun containsKey(key: K): Boolean = true
    override fun containsValue(value: String): Boolean = true
    override fun get(key: K): String? = "found"
    override val keys: Set<K> get() = throw UnsupportedOperationException("keys")
    override val values: Collection<String>
        get() = throw UnsupportedOperationException("values")
    override val entries: Set<Map.Entry<K, String>>
        get() = throw UnsupportedOperationException("entries")
}

fun generic(): Map<String?, String> = Generic<String?>()
fun words(): Map<String, String> = Words()
fun optional(): Map<String?, String?> = Optional()

// MODULE: main(library)
// FILE: main.kt
import maps.generic
import maps.optional
import maps.words

@Suppress("UNCHECKED_CAST")
fun box(): String {
    val wide = words() as Map<Any?, Any?>
    if (!wide.containsKey("a")) return "containsKey"
    if (!wide.containsValue("A")) return "containsValue"
    if (wide["a"] != "found") return "get"
    if (wide.containsKey(1)) return "wrong-type containsKey"
    if (wide.containsValue(1)) return "wrong-type containsValue"
    if (wide[1] != null) return "wrong-type get"
    if (wide.containsKey(null)) return "null containsKey"
    if (wide.containsValue(null)) return "null containsValue"
    if (wide[null] != null) return "null get"
    val maybe = optional() as Map<Any?, Any?>
    if (!maybe.containsKey(null) || !maybe.containsValue(null)) return "optional null"
    if (maybe[null] != "found") return "optional get"
    if (!maybe.containsKey("x") || maybe["x"] != "found") return "optional present"
    if (maybe.containsKey(1) || maybe.containsValue(1) || maybe[1] != null) return "optional wrong-type"
    val open = generic() as Map<Any?, Any?>
    if (!open.containsKey(null)) return "generic null containsKey"
    if (open[null] != "found") return "generic null get"
    return "OK"
}
