// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable lambda that captures a value, passed from a function that is not composable, has no
// composition to remember it in: it is `composableLambdaInstance(key, true, lambda)` built where it
// is passed.
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

import lib.Label
import lib.setContent

fun enter(text: String) {
    setContent { Label(text) }
}

// FILE: box.kt
package app

fun box(): String = "OK"
