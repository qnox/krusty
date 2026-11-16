// The stdlib's empty collections are private objects of their files: `emptyMap()` reads
// `EmptyMap`, `emptyList()` reads `EmptyList` and `emptySet()` reads `EmptySet`. Each read is the
// one instance of that object, and its members answer as an empty collection.
fun box(): String {
    val map: Map<String, Int> = emptyMap()
    if (map.size != 0 || !map.isEmpty()) return "FAIL map size"
    if (map["x"] != null || map.containsKey("x") || map.containsValue(1)) return "FAIL map lookup"
    if (map.keys.size != 0 || map.values.size != 0 || map.entries.size != 0) return "FAIL map views"
    if (map != emptyMap<String, Int>()) return "FAIL map equality"
    if (map.toString() != "{}" || map.hashCode() != 0) return "FAIL map string"

    val list: List<String> = emptyList()
    if (list.size != 0 || !list.isEmpty() || list.contains("x")) return "FAIL list"
    if (list.indexOf("x") != -1 || list.toString() != "[]" || list.hashCode() != 1) {
        return "FAIL list members"
    }
    if (list != emptyList<String>()) return "FAIL list equality"

    val set: Set<String> = emptySet()
    if (set.size != 0 || set.contains("x") || set.toString() != "[]" || set.hashCode() != 0) {
        return "FAIL set"
    }
    return "OK"
}
