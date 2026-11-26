// A library's generic top-level extension properties, read through their getters from another
// module: directly, bounded, from another library function, and through a setter.

// MODULE: lib
// FILE: Properties.kt
package props

class Box<T>(var item: T)

class Span<T>(val low: T, val high: T)

val <T> Array<out T>.lastOrNullItem: T? get() = getOrNull(lastIndex)

val <T : Comparable<T>> Span<T>.ordered: Boolean get() = low <= high

var <T> Box<T>.content: T
    get() = item
    set(value) {
        item = value
    }

fun <T> lastOrDefault(values: Array<out T>, default: T): T = values.lastOrNullItem ?: default

// MODULE: main(lib)
// FILE: Main.kt
import props.*

fun box(): String {
    if (arrayOf("x", "y").lastOrNullItem != "y") return "last"
    if (arrayOf<Int>().lastOrNullItem != null) return "empty"
    if (!Span(1, 2).ordered || Span("b", "a").ordered) return "bounded"
    val box = Box("O")
    if (box.content != "O") return "getter"
    box.content = "K"
    if (box.item != "K" || box.content != "K") return "setter"
    if (lastOrDefault(arrayOf(1, 2), 0) != 2 || lastOrDefault(arrayOf<Int>(), 5) != 5) {
        return "library call"
    }
    return "OK"
}
