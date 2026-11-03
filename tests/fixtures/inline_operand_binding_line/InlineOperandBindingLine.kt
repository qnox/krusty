class Pool(val name: String, val size: Int)

fun drained(current: List<Pool>, names: Set<String>): List<String> {
    val out = mutableListOf<String>()
    current
        .filter { it.name !in names }
        .forEach { pool ->
            out += pool.name
        }
    return out
}

fun oneLine(current: List<Pool>, names: Set<String>): List<String> {
    val out = mutableListOf<String>()
    current.filter { it.name in names }.forEach { out += it.name }
    return out
}

fun mapped(current: List<Pool>): Int {
    var total = 0
    current
        .map { it.size }
        .forEach { total += it }
    return total
}

inline fun <T> take(items: List<T>): List<T> {
    val size = items.size
    return items.subList(0, size)
}

fun kept(current: List<Pool>, names: Set<String>): Int {
    val kept = take(
        current.filter { it.name !in names }
    )
    return kept.size
}

fun takenStatement(current: List<Pool>, names: Set<String>): Int {
    var count = 0
    take(current.filter { it.name in names })
    count += 1
    return count
}

fun box(): String {
    val pools = listOf(Pool("a", 1), Pool("b", 2), Pool("c", 3))
    val result = listOf(
        drained(pools, setOf("b")).toString(),
        oneLine(pools, setOf("b")).toString(),
        mapped(pools).toString(),
        kept(pools, setOf("b")).toString(),
        takenStatement(pools, setOf("b")).toString(),
    ).joinToString("|")
    return if (result == "[a, c]|[b]|6|2|1") "OK" else result
}
