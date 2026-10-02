// Kotlin's answers for `list_api_answers.c`: the list, walk and array entry points on the ordinary
// path. `T` is the program class the driver's `Tag` (`program_collections.h`) stands in for.
class T(val n: Int) {
    override fun equals(other: Any?) = other is T && other.n == n
    override fun hashCode() = n
    override fun toString() = "T$n"
}

fun box(): String = buildString {
    val xs = listOf(T(1), T(2), T(3))
    appendLine("$xs ${xs.size} ${xs[1]} ${xs.first()} ${xs.last()} ${xs.indexOf(T(2))} " +
        "${xs.lastIndexOf(T(3))} ${xs.contains(T(4))} ${xs.hashCode()} " +
        "${xs == listOf(T(1), T(2), T(3))} ${xs == listOf(T(1), T(2))}")
    val m = mutableListOf(T(1))
    m.add(T(2))
    m.add(0, T(0))
    val old = m.set(1, T(5))
    val removed = m.removeAt(0)
    appendLine("$m $old $removed ${m.remove(T(5))} ${m.remove(T(9))} ${m.size}")
    m.addAll(listOf(T(3), T(4)))
    m += T(6)
    appendLine("$m ${m == listOf(T(2), T(3), T(4), T(6))} ${m.hashCode()}")
    m.sortWith { a, b -> b.n - a.n }
    appendLine(m)
    m.clear()
    appendLine("$m ${m.isEmpty()}")
    val ns = listOf(3, 1, 2)
    appendLine("${ns.map { it * 2 }} ${ns.filter { it > 1 }} ${ns.filterNot { it > 1 }} " +
        "${ns.any { it > 2 }} ${ns.all { it > 0 }} ${ns.none { it > 5 }} ${ns.count()} " +
        "${ns.count { it > 1 }}")
    appendLine("${ns.first { it < 3 }} ${ns.firstOrNull { it > 5 }} ${ns.last { it > 1 }} " +
        "${ns.fold(0) { a, b -> a + b }} ${ns.toList()} ${ns.reversed()} " +
        "${ns.sortedWith { a, b -> a - b }}")
    val seen = StringBuilder()
    ns.forEachIndexed { i, v -> seen.append("$i:$v,") }
    appendLine("$seen ${ns.joinToString()} ${ns + 4} ${ns + listOf(5, 6)} ${ns.sumOf { it }} " +
        "${ns.sumOf { it.toLong() * 3000000000L }} ${ns.sumOf { it / 2.0 }}")
    appendLine("${ns.withIndex().toList()} ${ns.isEmpty()} ${ns.isNotEmpty()} " +
        "${listOf<Int>().any()}")
    val iv = IndexedValue(1, T(2))
    appendLine("$iv ${iv.hashCode()} ${iv == IndexedValue(1, T(2))} " +
        "${iv == IndexedValue(2, T(2))}")
    val ia = intArrayOf(1, 2, 3)
    appendLine("${ia.toList()} ${ia.reversed()} ${ia.reversedArray().contentToString()} " +
        "${ia.isEmpty()} ${intArrayOf().isNotEmpty()}")
    val ta = arrayOf(T(1), T(2))
    appendLine("${ta.contentEquals(arrayOf(T(1), T(2)))} ${ta.contentHashCode()} " +
        "${ta.contentToString()} ${ta.toList()} " +
        "${listOf(1, 2).toTypedArray().contentToString()}")
    appendLine("${(1..4 step 2).map { it }} ${(3 downTo 1).toList()} " +
        "${('a'..'c').joinToString()} ${(1..3).count()} ${"héllo".map { it }} " +
        "${"ab".toList()}")
    appendLine("${listOf(T(1)) + T(2)} ${listOf(1, null, 3)}")
}
