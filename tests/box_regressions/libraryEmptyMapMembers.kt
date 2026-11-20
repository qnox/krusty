// The standard library's emptyMap() is a library-private object implementing Map. Its members are
// reached through Map-typed receivers, and it meets the runtime's own empty map in equality.

fun box(): String {
    val empty: Map<String, Int> = emptyMap()
    if (!empty.isEmpty()) return "isEmpty"
    if (empty.size != 0) return "size ${empty.size}"
    if (empty.containsKey("a")) return "containsKey"
    if (empty.containsValue(1)) return "containsValue"
    if (empty["a"] != null) return "get"
    if (empty.keys.size != 0) return "keys"
    if (empty.entries.size != 0) return "entries"
    if (empty.toString() != "{}") return "toString ${empty}"
    if (empty !== emptyMap<Int, String>()) return "identity"
    if (empty != mapOf<String, Int>()) return "equals"
    if (empty.hashCode() != 0) return "hashCode"
    return "OK"
}
