// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A call of a library composable with a composable function type that takes a receiver or an
// argument passes a lambda written for it like a call of the file's own function does.
// MODULE: lib
// FILE: lib.kt
package lib

import androidx.compose.runtime.Composable

@Composable
fun Section(content: @Composable String.() -> Unit) {
    "section".content()
}

@Composable
fun Rows(row: @Composable (Int) -> Unit) {
    row(1)
}

@Composable
fun Label(text: String) {}

// MODULE: main(lib)
// FILE: screen.kt
package app

import androidx.compose.runtime.Composable
import lib.Label
import lib.Rows
import lib.Section

@Composable
fun Screen(title: String) {
    Section { Label(this) }
    Rows { Label(title) }
    Rows { Label("static") }
}

// FILE: box.kt
package app

fun box(): String = "OK"
