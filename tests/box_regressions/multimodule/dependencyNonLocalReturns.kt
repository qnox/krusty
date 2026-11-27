// MODULE: library
// FILE: Library.kt
package fixture

inline fun loopOn(start: Int, block: (Int) -> Unit): Nothing {
    var value = start
    while (true) {
        block(value)
        value += 1
    }
}

inline fun each(values: IntArray, block: (Int) -> Unit) {
    for (value in values) block(value)
}

class Counter(private val values: IntArray) {
    fun firstAbove(limit: Int): Int {
        loopOn(0) { index ->
            if (values[index] > limit) return values[index]
        }
    }

    fun firstPairAbove(limit: Int): Int {
        each(values) { first ->
            each(values) { second ->
                if (first + second > limit) return first * 10 + second
            }
        }
        return -1
    }

    fun countBelow(limit: Int): Int {
        val count = IntArray(1)
        each(values) { value ->
            if (value >= limit) return@each
            count[0] += 1
        }
        return count[0]
    }
}

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val counter = Counter(intArrayOf(3, 8, 5, 9))
    if (counter.firstAbove(4) != 8) return "fail: firstAbove ${counter.firstAbove(4)}"
    if (counter.firstPairAbove(15) != 88) return "fail: firstPairAbove ${counter.firstPairAbove(15)}"
    if (counter.firstPairAbove(20) != -1) return "fail: no pair ${counter.firstPairAbove(20)}"
    return if (counter.countBelow(6) == 2) "OK" else "fail: countBelow ${counter.countBelow(6)}"
}
