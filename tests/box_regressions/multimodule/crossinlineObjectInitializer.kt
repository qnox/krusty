// A `crossinline` lambda that a dependency inline function's anonymous object invokes while it is
// constructed, from a property initializer or an `init` block, is inlined into the object's
// constructor at the call.
// MODULE: lib
// FILE: Initializing.kt
package lib

inline fun counted(crossinline text: () -> String) =
    object {
        val value = text()
        var count = 0

        init {
            count = value.length
        }

        fun grow(): Int {
            count += 1
            return count
        }

        override fun toString() = value + grow()
    }

inline fun both(crossinline text: () -> String) =
    object {
        val first = text()

        override fun toString() = first + text()
    }

// MODULE: main(lib)
// FILE: Main.kt
import lib.*

fun counting(prefix: String) = counted { prefix + "!" }

fun twice(word: String) = both { word }

fun box(): String {
    val result = counting("O").toString() + counting("KK").toString() + twice("a").toString()
    return if (result == "O!3KK!4aa") "OK" else result
}
