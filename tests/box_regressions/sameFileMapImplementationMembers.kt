// A class of this file implements Map and is read through Map-typed receivers in the same file. The
// members with a special bridge (get, containsKey, containsValue) answer their neutral result for an
// argument of the wrong erased type instead of reaching the override.

class Single(private val k: String, private val v: String) : Map<String, String> {
    override val size: Int get() = 1
    override val keys: Set<String> get() = throw UnsupportedOperationException()
    override val values: Collection<String> get() = throw UnsupportedOperationException()
    override val entries: Set<Map.Entry<String, String>> get() = throw UnsupportedOperationException()
    override fun isEmpty(): Boolean = false
    override fun containsKey(key: String): Boolean = key == k
    override fun containsValue(value: String): Boolean = value == v
    override fun get(key: String): String? = if (key == k) v else null
}

fun box(): String {
    val m: Map<String, String> = Single("O", "K")
    if (m.size != 1) return "size"
    if (m.isEmpty()) return "isEmpty"
    if (!m.containsKey("O")) return "containsKey"
    if (m.containsKey("x")) return "containsKey miss"
    if (!m.containsValue("K")) return "containsValue"
    if (m["x"] != null) return "get miss"
    val wide = m as Map<Any?, Any?>
    if (wide.containsKey(1)) return "wide containsKey"
    if (wide.containsValue(1)) return "wide containsValue"
    if (wide[1] != null) return "wide get"
    if (wide.containsKey(null)) return "null containsKey"
    if (wide.containsValue(null)) return "null containsValue"
    if (wide[null] != null) return "null get"
    return "O" + m["O"]!!
}
