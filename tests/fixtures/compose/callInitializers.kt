// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable call that initializes a local `val` of a composable body is a call of the body like a
// statement one: it takes the composer and its `$changed` bits, and its source information lists it.
// FILE: callInitializers.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Next(value: Int): Int = value + 1

@Composable
fun Show(text: String, count: Int) {}

@Composable
fun Frame(content: @Composable () -> Unit) {
    content()
}

@Composable
fun Screen(label: String, count: Int) {
    val unused = Next(count)
    Show(label, count)
    Frame {
        val inner = Next(1)
        Show("in", 1)
    }
}

// FILE: box.kt
package screens

fun box(): String = "OK"
