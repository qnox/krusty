// A runtime list's bulk operations read their argument through its own `iterator()` and
// `contains`, whatever collection it is: another list or a class of the program.
class Evens(private val limit: Int) : Collection<Int> {
    override val size: Int get() = limit
    override fun isEmpty() = limit == 0
    override fun iterator(): Iterator<Int> = object : Iterator<Int> {
        var next = 0
        override fun hasNext() = next < limit
        override fun next() = 2 * next++
    }
    override fun contains(element: Int) = element % 2 == 0 && element < 2 * limit
    override fun containsAll(elements: Collection<Int>): Boolean {
        for (element in elements) if (!contains(element)) return false
        return true
    }
}

fun elements(list: List<Int>): String {
    var text = ""
    for (element in list) text += "$element,"
    return text
}

fun listOfInts(vararg values: Int): ArrayList<Int> {
    val list = ArrayList<Int>()
    for (value in values) list.add(value)
    return list
}

fun box(): String {
    val copy = ArrayList(Evens(2))
    if (elements(copy) != "0,2,") return "constructor: ${elements(copy)}"
    val list = ArrayList<Int>()
    list.addAll(Evens(3))
    if (elements(list) != "0,2,4,") return "addAll: ${elements(list)}"
    if (!list.containsAll(Evens(2))) return "containsAll"
    if (list.containsAll(listOfInts(1))) return "containsAll of a list"
    list.addAll(listOfInts(5, 6))
    list.removeAll(Evens(10))
    if (elements(list) != "5,") return "removeAll: ${elements(list)}"
    val kept = listOfInts(1, 2, 3, 4)
    kept.retainAll(Evens(2))
    if (elements(kept) != "2,") return "retainAll: ${elements(kept)}"
    return "OK"
}
