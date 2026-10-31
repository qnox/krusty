// Without `@file:JvmName`, kotlinc ignores `@file:JvmMultifileClass`: the file compiles to its
// ordinary package part, private functions keep their names, and callers name that part.
// FILE: plain.kt
@file:kotlin.jvm.JvmMultifileClass
package plain

private fun hidden() = "O"

fun visible() = hidden() + "K"

// FILE: main.kt
fun box(): String = plain.visible()
