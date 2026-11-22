// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A plain lambda written inside a composable function is remembered, so a recomposition passes the
// callee the same instance.
// FILE: plainLambdas.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Button(onClick: () -> Unit) {}

@Composable
fun Screen() {
    Button { }
}

fun box(): String = "OK"
