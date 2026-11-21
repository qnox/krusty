fun box(): String {
    val growable = mutableListOf(1, 2, 3, 4)
    val readable: List<Int> = growable
    val outer = readable.subList(0, 3)
    val inner = outer.subList(1, 2)
    val walk = outer.iterator()
    growable.add(5)
    try {
        walk.next()
        return "walk"
    } catch (e: ConcurrentModificationException) {
    }
    try {
        outer.size
        return "outer"
    } catch (e: ConcurrentModificationException) {
    }
    try {
        inner.size
        return "inner size"
    } catch (e: ConcurrentModificationException) {
    }
    try {
        inner[0]
        return "inner get"
    } catch (e: ConcurrentModificationException) {
    }
    return "OK"
}
