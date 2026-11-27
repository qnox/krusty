// A public top-level `lateinit` property is read from another file of the module: the read of an
// uninitialized one throws a Kotlin exception naming the property, and a read after a write from
// either file answers the written value.
// FILE: Pending.kt
package pending

lateinit var message: String

fun fill(value: String) {
    message = value
}

// FILE: Main.kt
import pending.fill
import pending.message

fun box(): String {
    try {
        message
        return "FAIL: uninitialized read returned"
    } catch (e: RuntimeException) {
        if (e.message != "lateinit property message has not been initialized") return "FAIL: ${e.message}"
    }
    fill("O")
    val first = message
    if (first != "O") return "FAIL: read after fill: $first"
    message = first + "K"
    return message
}
