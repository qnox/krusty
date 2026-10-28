// An explicit import of a function named like a default-imported class leaves the class visible
// in type positions; the call still selects the imported function.
// FILE: lib.kt
package lib

fun String(flag: Boolean): kotlin.String = if (flag) "yes" else "no"

// FILE: main.kt
import lib.String

fun make(flag: Boolean): String? = String(flag)

fun String.isYes(): Boolean = length == 3

fun box(): String {
    val answer = make(true) ?: return "null"
    return if (answer.isYes()) "OK" else "FAIL: $answer"
}
