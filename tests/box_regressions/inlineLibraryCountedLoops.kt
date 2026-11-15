// Inlined library bodies that loop over a range literal (`repeat`: `0 until times`), over
// `indices` (`indexOfFirst`) and over `indices.reversed()` (`indexOfLast`).
fun box(): String {
    var count = 0
    repeat(3) { count += it + 1 }
    if (count != 6) return "repeat: $count"
    var none = 0
    repeat(0) { none++ }
    repeat(-2) { none++ }
    if (none != 0) return "empty repeat: $none"

    val values = arrayOf("a", "b", "a", "c")
    val first = values.indexOfFirst { it == "a" }
    if (first != 0) return "indexOfFirst: $first"
    val last = values.indexOfLast { it == "a" }
    if (last != 2) return "indexOfLast: $last"
    if (values.indexOfFirst { it == "z" } != -1) return "indexOfFirst missing"
    if (values.indexOfLast { it == "z" } != -1) return "indexOfLast missing"
    return "OK"
}
