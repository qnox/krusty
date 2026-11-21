// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable function value that takes a receiver or arguments is called with them before the
// composer, and a lambda written for it takes them before the composer too.
// FILE: receiverLambdas.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Leaf() {}

@Composable
fun Item(label: String) {}

@Composable
fun Box(content: @Composable String.() -> Unit) {
    "scope".content()
}

@Composable
fun Scoped(text: String, content: @Composable String.() -> Unit) {
    text.content()
}

@Composable
fun Indexed(item: @Composable (Int) -> Unit) {
    item(1)
}

@Composable
fun Captured(title: String) {
    Scoped("b") { Item(title) }
    Indexed { Item(title) }
}

@Composable
fun Screen() {
    Box { Leaf() }
    Scoped("a") { Item(this) }
    Indexed { Leaf() }
}

// FILE: box.kt
package screens

fun box(): String = "OK"
