// An `ArrayList` held as one of the collection interfaces it implements runs its own members:
// a call of `add` through `MutableCollection`, `get` and `listIterator` through `List`, and the
// stdlib's `map`, `filter` and `single`, which reach the list the same way. An index outside the
// list throws `IndexOutOfBoundsException`, held as an `ArrayList` or as a `List`, and an element
// read from an `ArrayList<Int>` is an `Int`.
fun put(target: MutableCollection<Int>, value: Int): Boolean = target.add(value)

fun at(list: List<Int>, index: Int): Int = list[index]

fun rejects(read: () -> Any): Boolean =
    try {
        read()
        false
    } catch (e: IndexOutOfBoundsException) {
        true
    }

fun backwards(list: List<Int>): String {
    val iterator = list.listIterator(list.size)
    var text = ""
    while (iterator.hasPrevious()) {
        text += "" + iterator.previousIndex() + ":" + iterator.previous() + " "
    }
    return text + iterator.nextIndex()
}

fun box(): String {
    val list = ArrayList<Int>()
    if (!put(list, 4) || !put(list, 5)) return "FAIL add answers true"
    if (list.size != 2 || at(list, 1) != 5) return "FAIL added: ${list.size}"
    if (list[0] + 1 != 5) return "FAIL ArrayList.get: ${list[0]}"
    if (!rejects { at(list, 2) } || !rejects { at(list, -1) }) return "FAIL List.get bounds"
    if (!rejects { list[2] }) return "FAIL ArrayList.get bounds"
    if (!rejects { list.listIterator(3) }) return "FAIL listIterator bounds"
    if (backwards(list) != "1:5 0:4 0") return "FAIL backwards: ${backwards(list)}"
    val mapped = listOf(1, 2).map { it + 1 }.filter { it > 2 }
    if (mapped.single() != 3) return "FAIL mapped"
    return "OK"
}
