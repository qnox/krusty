// WITH_STDLIB
// WITH_COMPOSE
// Parameterless composable functions become restartable, skippable groups with source
// information, trace markers and a durable key, and pass their composer to each other.
package screens

import androidx.compose.runtime.Composable

@Composable
fun Leaf() {
}

@Composable
fun Screen() {
    Leaf()
    Leaf()
}

@Composable
fun Labelled() {
    val label = "screen"
    println(label)
    Screen()
}

fun box(): String = "OK"
