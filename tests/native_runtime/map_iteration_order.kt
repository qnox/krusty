// Kotlin's answers for `map_iteration_order.c`: the iteration order of each map and set kind and
// spelling, which for a `HashMap` or `HashSet` is its table's and so depends on how its capacity
// grew. `H` is the program class the driver's `Key` stands in for: a key of a given hash whose
// text is its number.
class H(val n: Int, val h: Int) {
    override fun hashCode() = h
    override fun equals(other: Any?) = other is H && other.n == n
    override fun toString() = "$n"
}

fun h(n: Int) = H(n, n)

fun box(): String = buildString {
    val hm = HashMap<Any, Int>()
    for (x in listOf(33, 1, 17, 16, 0, 49, 2)) hm[h(x)] = x
    appendLine(hm.keys)
    appendLine(hashMapOf("banana" to 1, "apple" to 2, "cherry" to 3, "date" to 4).keys)
    appendLine(hashSetOf(h(100), h(3), h(17), h(64), h(5)))
    val big = HashMap<Any, Int>()
    for (i in 0 until 13) big[h(i * 16)] = i
    appendLine(big.keys)
    val hs = HashSet<Any>()
    for (i in listOf(15, 31, 47, 63, 1)) hs.add(h(i))
    appendLine(hs)
    val coll = HashMap<Any, Int>()
    for (i in 0 until 10) coll[H(i, 5 + 32 * i)] = i
    appendLine(coll.keys)
    appendLine(hashSetOf(h(-1), h(-16), h(65536), h(65537), h(1)))
    val rm = hashMapOf(h(1) to "a", h(2) to "b", h(3) to "c")
    rm.remove(h(1))
    rm[h(1)] = "z"
    rm[h(17)] = "q"
    appendLine(rm.keys)
    appendLine(mapOf(h(17) to "c", h(1) to "a").keys)
    appendLine(HashMap<Any, Int>(0).apply { put(h(5), 5); put(h(1), 1); put(h(3), 3) }.keys)
    appendLine(HashSet<Any>(2).apply { add(h(9)); add(h(2)); add(h(4)) })
    appendLine(mutableMapOf(h(9) to 1, h(2) to 2).keys)
    appendLine(setOf(h(33), h(1), h(17)))
    for (capacity in listOf(-1, -7)) {
        val answer = try {
            if (capacity == -1) HashMap<Any, Any>(capacity) else HashSet<Any>(capacity)
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine(answer)
    }
}
