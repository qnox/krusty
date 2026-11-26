// LANGUAGE: +CollectionLiterals
// A collection literal passed for a type variable is built as the type the other arguments fix:
// `select(mutableSetOf<Int>(), [1])` is a `MutableSet<Int>` built by its `operator fun of`, not the
// `Collection<Int>` joined from the literal's expectation-free `List<Int>` reading.

fun <T> select(vararg x: T): T = x[x.size - 1]

fun box(): String {
    val set: MutableSet<Int> = select(mutableSetOf<Int>(), [1])
    if (set.size != 1 || 1 !in set) return "fail: $set"
    set.add(2)
    return if (set.size == 2) "OK" else "fail: $set"
}
