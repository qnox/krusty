// LANGUAGE: +CompanionBlocks
// A star import of a classifier brings its companion block's callables into scope; the class's
// instance members stay out of scope.

// FILE: a/Counter.kt
package a

class Counter {
    companion {
        fun start(): Int = 40
        fun plus(left: Int, right: Int): Int = left + right
    }

    fun instanceOnly(): Int = -1
}

// FILE: main.kt
import a.Counter.*

fun box(): String {
    val total = plus(start(), 2)
    return if (total == 42) "OK" else "Fail: $total"
}
