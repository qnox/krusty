// A library's top-level functions compute with constants, parameters and locals, call each other
// and the standard library, branch with `if` and `when`, test and cast types, compare and assert
// values not null, and build strings from templates. A program linked against the library's KLIB
// runs each body as the library wrote it.
// MODULE: lib
// FILE: Library.kt
package fixture

fun answer(): Int { return 42 }

fun sum(a: Int, b: Int): Int {
    val total = a + b
    var scaled = total
    scaled = scaled * 2
    return scaled - total
}

fun describe(value: Any?): String {
    if (value == null) return "null"
    return when (value) {
        is String -> "string ${value.length}"
        is Int -> "int $value"
        else -> "other"
    }
}

fun firstNonNull(a: String?, b: String): String = a ?: b

fun forced(a: String?): Int = a!!.length

fun greet(name: String): String = "Hello, $name!"

fun larger(a: Int, b: Int): Int = if (a > b) a else b

fun isNegative(a: Long): Boolean = a < 0L

fun differs(a: String, b: String): Boolean = a != b

fun twice(value: Int): Int = sum(value, value)

fun countdown(from: Int): String {
    var label = "go"
    if (from <= 0) {
        label = "done"
    }
    return label
}

// MODULE: main(lib)
// FILE: Main.kt
import fixture.*

fun box(): String {
    if (answer() != 42) return "answer"
    if (sum(40, 2) != 42) return "sum"
    if (describe(null) != "null") return "describe null"
    if (describe("abc") != "string 3") return "describe string"
    if (describe(7) != "int 7") return "describe int"
    if (describe(1.5) != "other") return "describe other"
    if (firstNonNull(null, "b") != "b" || firstNonNull("a", "b") != "a") return "elvis"
    if (forced("four") != 4) return "forced"
    try {
        forced(null)
        return "forced null"
    } catch (expected: NullPointerException) {
    }
    if (greet("krusty") != "Hello, krusty!") return "greet"
    if (larger(3, 9) != 9 || larger(9, 3) != 9) return "larger"
    if (!isNegative(-1L) || isNegative(1L)) return "isNegative"
    if (!differs("a", "b") || differs("a", "a")) return "differs"
    if (twice(21) != 42) return "twice"
    if (countdown(0) != "done" || countdown(3) != "go") return "countdown"
    return "OK"
}
