// TARGET_BACKEND: JVM_IR
// USE_INLINE_SCOPES_NUMBERS
// WITH_STDLIB

class Holder(var total: Int) {
    fun first(n: Int): Int {
        var acc = 0
        repeat(n) { acc += it }
        return acc
    }

    constructor(xs: List<Int>) : this(0) {
        val sum = xs.map { it + 1 }.sum()
        total = sum
    }
}

fun box(): String {
    val h = Holder(listOf(1, 2))
    return if (h.total == 5 && h.first(3) == 3) "OK" else "fail"
}
