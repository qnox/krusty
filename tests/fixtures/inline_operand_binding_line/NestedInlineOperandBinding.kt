class Pool(val name: String, val size: Int)

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
        kept(pools, setOf("b")).toString(),
        takenStatement(pools, setOf("b")).toString(),
    ).joinToString("|")
    return if (result == "2|1") "OK" else result
}
