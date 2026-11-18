// `buildList` ends with the standard library's `ArrayList.build()`: the builder's elements as a
// list that is still the library's `ArrayList`, so a `MutableList` cast holds and a write through
// it is refused.
fun refusesWrite(list: List<String>): Boolean {
    val mutable = list as? MutableList<String> ?: return false
    try {
        mutable.add("x")
    } catch (e: UnsupportedOperationException) {
        return true
    }
    return false
}

fun box(): String {
    val list = buildList {
        add("O")
        add("K")
    }
    if (list.size != 2) return "FAIL size ${list.size}"
    val empty = buildList<String> { }
    if (!empty.isEmpty()) return "FAIL empty"
    val sized = buildList(3) {
        add("1")
        add("2")
    }
    if (sized.size != 2) return "FAIL sized"
    if ((list as Any) !is MutableList<*>) return "FAIL result is not a MutableList"
    if (!refusesWrite(list)) return "FAIL write accepted"
    if (!refusesWrite(empty)) return "FAIL empty write accepted"
    if (list.size != 2) return "FAIL size after refused write ${list.size}"
    return list[0] + list[1]
}
