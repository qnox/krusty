// A receiver-less call whose nearest scope level declares only functions the call site cannot
// access continues outward: kotlinc never treats an invisible candidate as successful, so a
// file-private function in the current package or a dependency's internal one under a star import
// leaves the default-imported `kotlin.comparisons.minOf` selectable.
// MODULE: library
// FILE: library.kt
package library

internal fun minOf(a: Long, b: Int): Long = -1L

internal fun minOf(a: Int, b: Long): Long = -1L

// MODULE: main(library)
// FILE: hidden.kt
package app

private fun minOf(a: Long, b: Int): Long = -2L

private fun minOf(a: Int, b: Long): Long = -2L

// FILE: imported.kt
package imported

import library.*

fun smallerImported(a: Long, b: Long) = minOf(a, b)

fun smallestImported(a: Long, b: Long): Long {
    val first = minOf(a, b)
    return minOf(first, 3L)
}

// FILE: main.kt
package app

import imported.*

fun smallerInPackage(a: Long, b: Long) = minOf(a, b)

fun box(): String {
    val five = 5L
    val two = 2L
    if (smallerInPackage(five, two) != 2L) return "package: " + smallerInPackage(five, two)
    if (smallerImported(five, two) != 2L) return "imported: " + smallerImported(five, two)
    if (smallestImported(five, 4L) != 3L) return "nested: " + smallestImported(five, 4L)
    if (minOf(five, two) != 2L) return "body: " + minOf(five, two)
    return "OK"
}
