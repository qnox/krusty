// A library-private object read by different library functions that two source files call: both
// files reach the one instance, its state and its identity.

// MODULE: library
// FILE: library.kt
package privatecounter

private object Shared {
    var bumps = 0
}

fun bump(): Any {
    Shared.bumps++
    return Shared
}

fun bumps(): Int = Shared.bumps

fun shared(): Any = Shared

// MODULE: main(library)
// FILE: bumping.kt
import privatecounter.bump

fun bumpTwice(): Any {
    bump()
    return bump()
}

// FILE: main.kt
import privatecounter.bumps
import privatecounter.shared

fun box(): String {
    val bumped = bumpTwice()
    if (bumps() != 2) return "bumps ${bumps()}"
    if (bumped !== shared()) return "identity"
    return "OK"
}
