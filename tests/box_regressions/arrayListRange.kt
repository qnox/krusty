fun ints(vararg values: Int): ArrayList<Int> {
    val list = ArrayList<Int>()
    for (value in values) list.add(value)
    return list
}

fun box(): String {
    val growable = ints(1, 2, 3, 4)
    val readable: List<Int> = growable
    val range = readable.subList(1, 3)
    if (range.size != 2 || range[0] != 2 || range[1] != 3) return "initial"
    growable[1] = 20
    if (range[0] != 20) return "write through"
    if (range != ints(20, 3)) return "equals"
    if (!range.contains(3) || range.contains(1)) return "contains"
    var sum = 0
    for (value in range) sum += value
    if (sum != 23) return "iteration $sum"
    val inner = range.subList(1, 2)
    if (inner.size != 1 || inner[0] != 3) return "nested"

    growable.add(5)
    val fresh = readable.subList(0, 5)
    try {
        readable.subList(-1, 2)
        return "negative start"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        readable.subList(1, 9)
        return "end past size"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        readable.subList(3, 2)
        return "reversed"
    } catch (e: IllegalArgumentException) {
    }

    val appended = ints(1, 2, 3)
    val head = (appended as List<Int>).subList(0, 2)
    appended.addAll(head)
    if (appended != ints(1, 2, 3, 1, 2)) return "aliased addAll"
    if (fresh.size != 5) return "whole"
    return "OK"
}
