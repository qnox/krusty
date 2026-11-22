// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// JVM_TARGET: 11
// A call of the runtime's inline composable `remember` in a composable body is the runtime's
// already-transformed body spliced in, so it adds its own group, source information and cache call.
// FILE: screen.kt
package app

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember

val log = StringBuilder()

@Composable
fun Show(text: String, count: Int) {
    log.append(text).append(count)
}

@Composable
fun Counter(label: String) {
    val items = remember { listOf(1, 2) }
    Show(label, items.size)
}

// FILE: recorder.kt
package app

fun box(): String = "OK"
