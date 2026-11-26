// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A parameter of a function type that is not composable is stable, so a composable function
// compares it like any other stable parameter.
// FILE: functionTypeParameters.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Button(onClick: () -> Unit, label: String) {
    onClick()
}

@Composable
fun Mapped(transform: (Int) -> String, value: Int) {
    transform(value)
}

@Composable
fun Screen(onClick: () -> Unit, transform: (Int) -> String) {
    Button(onClick, "a")
    Mapped(transform, 1)
}

fun box(): String = "OK"
