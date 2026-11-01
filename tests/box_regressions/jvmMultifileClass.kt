// Two files share one multifile class: each compiles to a part, callers go through the facade,
// and private declarations of the same name in both parts do not collide.
// FILE: first.kt
@file:kotlin.jvm.JvmName("Shared")
@file:kotlin.jvm.JvmMultifileClass
package shared

const val GREETING = "O"
private const val HIDDEN = 1

private fun twice(value: Int) = value * 2

fun first(value: Int = 20) = twice(value) + HIDDEN

var late = ""

// FILE: second.kt
@file:kotlin.jvm.JvmName("Shared")
@file:kotlin.jvm.JvmMultifileClass
package shared

private fun twice(value: Int) = value + value + 1

fun second(value: Int) = twice(value)

private var counter = 0

fun bump(): Int {
    counter += 1
    return counter
}

// FILE: main.kt
import shared.*

fun box(): String {
    if (first() != 41) return "first"
    if (second(10) != 21) return "second"
    late = "K"
    if (bump() != 1 || bump() != 2) return "counter"
    return GREETING + late
}
