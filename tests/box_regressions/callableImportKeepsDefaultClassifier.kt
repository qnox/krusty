// An explicit import of a function named like a default-imported class leaves the class visible
// in type positions; the call still selects the imported function.
// FILE: lib.kt
package lib

fun Pair(value: Int): kotlin.Pair<Int, Int> = kotlin.Pair(value, value + 1)

// FILE: main.kt
import lib.Pair

fun make(value: Int): Pair<Int, Int>? = Pair(value)

fun Pair<Int, Int>.sum(): Int = first + second

fun box(): String {
    val pair = make(1) ?: return "null"
    return if (pair.sum() == 3) "OK" else "FAIL"
}
