// A list appending a range of itself differs by platform: kotlinc-native and kotlinc-wasm grow the
// list before they read the range and raise ConcurrentModificationException. That program is the
// ArrayListAddAllGrowsBeforeReadingItsArgument entry of tests/kotlinc_defects_e2e.rs.
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

    val written = ints(1, 2, 3, 4, 5)
    written.add(0, 0)
    if (written != ints(0, 1, 2, 3, 4, 5)) return "insert at an index $written"
    if (written.removeAt(0) != 0 || !written.remove(5)) return "remove at an index"
    if (written != ints(1, 2, 3, 4)) return "removed $written"
    val window = written.subList(1, 3)
    window.add(9)
    if (written != ints(1, 2, 3, 9, 4) || window.size != 3) return "add through a range $written"
    window.add(1, 7)
    if (written != ints(1, 2, 7, 3, 9, 4) || window != ints(2, 7, 3, 9)) return "insert through a range $written"
    if (window.removeAt(0) != 2 || !window.remove(9)) return "remove through a range"
    if (written != ints(1, 7, 3, 4) || window != ints(7, 3)) return "removed through a range $written"
    val pair = window.subList(1, 2)
    pair.add(8)
    if (written != ints(1, 7, 3, 8, 4) || window.size != 3 || pair.size != 2) return "nested add $written"
    pair.clear()
    if (written != ints(1, 7, 4) || window != ints(7) || pair.size != 0) return "nested clear $written"
    window.clear()
    if (written != ints(1, 4) || window.size != 0) return "clear through a range $written"
    val wider = written.subList(0, 2)
    wider.addAll(ints(5, 6))
    if (written != ints(1, 4, 5, 6)) return "addAll through a range $written"

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

    if (fresh.size != 5) return "whole"
    return "OK"
}
