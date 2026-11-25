// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// Composable calls that take a trailing composable lambda, nested in a composable body, as a
// layout does.
// MODULE: lib
// FILE: lib.kt
package lib

import androidx.compose.runtime.Composable

class Scope

@Composable
fun Column(content: @Composable Scope.() -> Unit) {}

@Composable
fun Text(text: String) {}

@Composable
fun Row(content: @Composable Scope.() -> Unit) {}

// MODULE: main(lib)
// FILE: screen.kt
package app

import androidx.compose.runtime.Composable
import lib.Column
import lib.Row
import lib.Text

@Composable
fun Screen(label: String) {
    Column {
        Text(label)
        Row {
            Text(label)
            Text("More")
        }
    }
}

@Composable
fun Static() {
    Column {
        Text("a")
        Row { Text("b") }
    }
}

@Composable
fun Mixed(label: String) {
    Column {
        Row { Text("b") }
        Row { Text(label) }
    }
}

// FILE: box.kt
package app

fun box(): String = "OK"
