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

    val root = ints(1, 2, 3, 4, 5)
    val view = root.subList(1, 4)
    val old = view.set(0, 20)
    if (old != 2 || root[1] != 20 || view[0] != 20) return "set through a range"
    val deeper = view.subList(1, 3)
    if (deeper.set(1, 40) != 4 || view[2] != 40 || root[3] != 40) return "set through a nested range"
    if (root != ints(1, 20, 3, 40, 5)) return "range writes $root"
    try {
        view.set(3, 0)
        return "set past the range"
    } catch (e: IndexOutOfBoundsException) {
    }
    if (root[4] != 5) return "set past the range wrote"

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
