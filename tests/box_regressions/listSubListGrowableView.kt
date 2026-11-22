fun box(): String {
    val growable = mutableListOf(1, 2, 3, 4)
    val readable: List<Int> = growable
    val range = readable.subList(1, 3)
    if (range.size != 2 || range[0] != 2 || range[1] != 3) return "initial"
    growable[1] = 20
    if (range[0] != 20) return "write through"
    if (range != listOf(20, 3)) return "equals"
    growable.add(5)
    try {
        range.size
        return "stale size"
    } catch (e: ConcurrentModificationException) {
    }
    try {
        range[0]
        return "stale get"
    } catch (e: ConcurrentModificationException) {
    }
    return "OK"
}
