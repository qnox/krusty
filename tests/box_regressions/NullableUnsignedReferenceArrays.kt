// WITH_STDLIB
fun box(): String {
    val filled = Array(2) { 7u }
    filled[1] = 9u
    if (filled[0] != 7u) return "filled[0]"
    if (filled[1] != 9u) return "filled[1]"
    val empty: Array<UInt?> = Array(2) { null }
    empty[0] = 5u
    if (empty[0] != 5u) return "empty[0]"
    if (empty[1] != null) return "empty[1]"
    val literal = arrayOf<UInt?>(1u, null)
    var sum = 0u
    for (x in literal) sum += x ?: 0u
    if (sum != 1u) return "sum"
    return "OK"
}
