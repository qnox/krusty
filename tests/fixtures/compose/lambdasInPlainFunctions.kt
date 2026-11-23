// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable lambda passed to a function from a function that is not composable, as an
// activity's `setContent { Screen() }` does, is a composable lambda like one passed in a composable
// body: it is a singleton when it captures nothing. A plain function's composable function-type
// parameter is the frontend's `ComposableFunction0` in the durable keys of the lambdas it passes,
// as a composable function's is.
// MODULE: lib
// FILE: lib.kt
package lib

import androidx.compose.runtime.Composable

fun setContent(content: @Composable () -> Unit) {}

@Composable
fun Label(text: String) {}

// MODULE: main(lib)
// FILE: plainFunctions.kt
package app

import androidx.compose.runtime.Composable
import lib.Label
import lib.setContent

fun enter() {
    setContent { Label("static") }
}

// Plain functions of the module that take composable lambdas, and hand them on.
fun keep(content: @Composable () -> Unit) {}

fun host(title: String, content: @Composable () -> Unit) {
    keep(content)
}

fun open(count: Int, content: @Composable () -> Unit) {
    host("plain") { Label("from a plain body") }
    host("again", content)
}

@Composable
fun Screen() {
    host("composable") { Label("from a composable body") }
}

// FILE: box.kt
package app

fun box(): String = "OK"
