fun box(): String {
    val list = listOf(10, 20, 30, 40, 50)
    val middle = list.subList(1, 4)
    if (middle.size != 3) return "size ${middle.size}"
    if (middle[0] != 20 || middle[2] != 40) return "get"
    if (middle != listOf(20, 30, 40)) return "equals"
    if (middle.hashCode() != listOf(20, 30, 40).hashCode()) return "hashCode"
    if (middle.toString() != "[20, 30, 40]") return "toString $middle"
    if (middle.indexOf(30) != 1 || !middle.contains(40) || middle.contains(10)) return "search"

    var sum = 0
    for (value in middle) sum += value
    if (sum != 90) return "iteration $sum"

    val inner = middle.subList(1, 3)
    if (inner != listOf(30, 40)) return "nested $inner"
    if (list.subList(2, 2).isNotEmpty()) return "empty"
    if (list.subList(0, 5) != list) return "whole"

    try {
        list.subList(-1, 2)
        return "negative start"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        list.subList(1, 6)
        return "end past size"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        list.subList(3, 2)
        return "reversed"
    } catch (e: IllegalArgumentException) {
    }
    try {
        middle[3]
        return "get past view"
    } catch (e: IndexOutOfBoundsException) {
    }
    return "OK"
}
