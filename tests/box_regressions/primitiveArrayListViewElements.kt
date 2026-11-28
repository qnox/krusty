// WITH_STDLIB

// A primitive array's `asList()` view is a stdlib local class whose `get`, `indexOf`,
// `lastIndexOf` and `contains` override the generic `List` members with primitive-carried ones;
// calling them through `List<Int>` reaches those overrides through the interface's bridges.
fun box(): String {
    val list: List<Int> = IntArray(3) { it + 5 }.asList()
    if (list.get(1) != 6) return "get: ${list.get(1)}"
    if (list[2] != 7) return "index: ${list[2]}"
    if (list.indexOf(6) != 1) return "indexOf: ${list.indexOf(6)}"
    if (list.indexOf(8) != -1) return "indexOf missing: ${list.indexOf(8)}"
    if (list.lastIndexOf(5) != 0) return "lastIndexOf: ${list.lastIndexOf(5)}"
    if (list.lastIndexOf(4) != -1) return "lastIndexOf missing: ${list.lastIndexOf(4)}"
    if (!list.contains(7) || list.contains(9)) return "contains"
    if (list.size != 3 || list.isEmpty()) return "size: ${list.size}"
    return "OK"
}
