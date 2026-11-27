// `buildList` ends with the standard library's `ArrayList.build()`: the builder's elements as a
// read-only list. The list still casts to `MutableList`, as kotlinc's does on every platform, and a
// write through the cast is refused.
fun box(): String {
    val list = buildList {
        add("O")
        add("K")
    }
    if (list.size != 2) return "FAIL size ${list.size}"
    try {
        (list as MutableList<String>).add("x")
        return "FAIL write accepted"
    } catch (e: UnsupportedOperationException) {
    }
    try {
        (list as MutableList<String>)[0] = "x"
        return "FAIL set accepted"
    } catch (e: UnsupportedOperationException) {
    }
    if (list.size != 2) return "FAIL size after refused writes ${list.size}"
    val empty = buildList<String> { }
    if (!empty.isEmpty()) return "FAIL empty"
    val sized = buildList(3) {
        add("1")
        add("2")
    }
    if (sized.size != 2) return "FAIL sized"
    if (sized[0] + sized[1] != "12") return "FAIL sized elements"
    return list[0] + list[1]
}
