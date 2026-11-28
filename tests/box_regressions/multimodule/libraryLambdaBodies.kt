// A library's top-level functions pass, return and invoke lambdas and callable references, and
// lambdas read and write the variables and parameters of the function that declares them. A program
// linked against the library's KLIB runs each body as the library wrote it.
// MODULE: lib
// FILE: Library.kt
package fixture

fun applyTo(value: Int, transform: (Int) -> Int): Int = transform(value)

fun doubled(value: Int): Int = applyTo(value) { it * 2 }

fun offset(value: Int, by: Int): Int = applyTo(value) { it + by }

fun negate(value: Int): Int = -value

fun negated(value: Int): Int = applyTo(value, ::negate)

fun adder(by: Int): (Int) -> Int = { it + by }

fun combine(first: String, second: String, join: (String, String) -> String): String =
    join(first, second)

fun joinedWith(separator: String): String = combine("a", "b") { left, right -> left + separator + right }

fun counted(times: Int): Int {
    var count = 0
    val step = { count = count + 1 }
    var i = 0
    while (i < times) {
        step()
        i = i + 1
    }
    return count
}

fun accumulated(values: IntArray): String {
    var total = 0
    var log = ""
    val add = { value: Int ->
        total = total + value
        log = log + value
    }
    for (value in values) add(value)
    return "$log=$total"
}

fun thunk(text: String): () -> String = { text + text }

fun nested(base: Int): Int {
    var seen = 0
    val outer = { a: Int ->
        val inner = { b: Int -> seen = seen + a + b }
        inner(base)
        seen
    }
    return outer(1) + outer(2)
}

// MODULE: main(lib)
// FILE: Main.kt
import fixture.*

fun box(): String {
    if (doubled(21) != 42) return "doubled"
    if (offset(40, 2) != 42) return "offset"
    if (negated(5) != -5) return "negated"
    if (adder(3)(4) != 7) return "adder"
    if (joinedWith("-") != "a-b") return "joinedWith"
    if (counted(4) != 4) return "counted"
    if (accumulated(intArrayOf(1, 2, 3)) != "123=6") return "accumulated"
    if (thunk("ab")() != "abab") return "thunk"
    if (nested(10) != 11 + 23) return "nested"
    return "OK"
}
