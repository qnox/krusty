// A library's top-level functions declare and pass `vararg` parameters, spread arrays into them,
// and build, read and write arrays. A program linked against the library's KLIB runs each body as
// the library wrote it.
// MODULE: lib
// FILE: Library.kt
package fixture

fun count(vararg values: Int): Int = values.size

fun total(vararg values: Int): Int {
    var sum = 0
    for (value in values) sum = sum + value
    return sum
}

fun words(vararg parts: String): String {
    var text = ""
    for (part in parts) text = text + part
    return text
}

fun listed(): Int = total(1, 2, 3)

fun spread(values: IntArray): Int = total(10, *values, 20)

fun none(): Int = 0 + count()

fun spelled(): String = words("a", "b", "c")

fun built(): Int = intArrayOf(4, 5, 6).size + arrayOf("x", "y").size

fun squares(n: Int): IntArray {
    val result = IntArray(n)
    var i = 0
    while (i < n) {
        result[i] = i * i
        i = i + 1
    }
    return result
}

fun second(values: Array<String>): String = values[1]

fun swapped(values: IntArray): IntArray {
    val first = values[0]
    values[0] = values[1]
    values[1] = first
    return values
}

// MODULE: main(lib)
// FILE: Main.kt
import fixture.*

fun box(): String {
    if (listed() != 6) return "listed"
    if (spread(intArrayOf(1, 2)) != 33) return "spread"
    if (none() != 0) return "none"
    if (spelled() != "abc") return "spelled"
    if (count(1, 2) != 2) return "count"
    if (built() != 5) return "built"
    val squares = squares(4)
    if (squares.size != 4 || squares[3] != 9) return "squares"
    if (second(arrayOf("a", "b")) != "b") return "second"
    val swapped = swapped(intArrayOf(1, 2))
    if (swapped[0] != 2 || swapped[1] != 1) return "swapped"
    return "OK"
}
