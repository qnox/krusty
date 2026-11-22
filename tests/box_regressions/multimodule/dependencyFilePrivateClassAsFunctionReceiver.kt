// MODULE: library
// FILE: Library.kt
package fixture

open class Scope(private val label: String) {
    fun describe(): String = "scope:" + label
}

private class Runner<T>(val value: T, label: String) : Scope(label)

fun <R, T> R.start(block: R.() -> T): T = block()

fun <T> launch(value: T, block: Scope.() -> T): T {
    val runner = Runner(value, "runner")
    return runner.start(block)
}

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val described = launch("x") { describe() }
    if (described != "scope:runner") return "fail: receiver $described"
    val number = launch(7) { 7 * 6 }
    return if (number == 42) "OK" else "fail: result $number"
}
