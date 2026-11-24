// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable function takes a class of another module that carries an annotation whose class the
// compiling module cannot see: kotlinc finds no stable marker on it and infers the class's stability
// from its fields.
// MODULE: ann
// FILE: ann.kt
package ann

@Retention(AnnotationRetention.BINARY)
annotation class Tag

// MODULE: lib(ann)
// FILE: lib.kt
package lib

import ann.Tag

@Tag
data class State(val label: String, val count: Int)

// MODULE: main(lib)
// FILE: screen.kt
package app

import androidx.compose.runtime.Composable
import lib.State

@Composable
fun Screen(initial: State) {}

// FILE: box.kt
package app

fun box(): String = "OK"
