// WITH_STDLIB
// One library function walks a range by index and then an array of a generic class. The range
// is an index progression; the array loop reads each element.
class Item<T>(val value: T)

fun <T> walk(items: Array<Item<T>>, start: Int, end: Int, wanted: T): Int {
    var found = -1
    for (index in start until end) {
        if (items[index].value == wanted) found = index
    }
    var matches = 0
    for (item in items) {
        if (item.value == wanted) matches += 1
    }
    return found + matches
}

fun box(): String {
    val items = arrayOf(Item("a"), Item("b"), Item("c"))
    if (walk(items, 0, items.size, "b") != 2) return "both"
    if (walk(items, 0, 3, "a") != 1) return "first"
    if (walk(items, 0, items.size, "z") != -1) return "missing"
    if (walk(items, 1, 2, "c") != 0) return "window"
    return "OK"
}
