interface Indexed {
    operator fun get(index: Int): Int
    infix fun plus(other: Int): Int
}

class IndexedImpl(private val base: Int) : Indexed {
    override fun get(index: Int): Int = base + index
    override fun plus(other: Int): Int = base + other
}

class Delegating(indexed: Indexed) : Indexed by indexed

fun box(): String {
    val d = Delegating(IndexedImpl(40))
    if (d[2] != 42) return "fail get: ${d[2]}"
    val sum = d plus 2
    return if (sum == 42) "OK" else "fail plus: $sum"
}
