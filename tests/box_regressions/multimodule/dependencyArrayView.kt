// MODULE: library
// FILE: library.kt
package dependencyview

fun <T> dependencyArrayView(values: Array<T>): Array<out Any?> = values

// MODULE: main(library)
// FILE: main.kt
import dependencyview.dependencyArrayView

private var evaluations = 0
private val input = arrayOf("OK")
private fun produce(): Array<String> {
    evaluations++
    return input
}

fun box(): String {
    val view = dependencyArrayView(produce())
    return if (evaluations == 1 && view === input && view[0] === input[0]) "OK" else "FAIL"
}
