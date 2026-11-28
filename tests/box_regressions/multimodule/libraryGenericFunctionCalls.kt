// A library's top-level function calls a generic function of its own file with an explicit type
// argument. A program linked against the library's KLIB runs the call as the library wrote it.
// MODULE: lib
// FILE: Library.kt
package fixture

fun <T> answer(): Int = 42

fun callsAnswer(): Int = answer<String>()

// MODULE: main(lib)
// FILE: Main.kt
import fixture.*

fun box(): String = if (callsAnswer() == 42) "OK" else "callsAnswer"
