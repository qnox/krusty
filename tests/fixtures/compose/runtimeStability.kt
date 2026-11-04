// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A class whose fields are of another file's classes, or of a dependency class compiled with
// Compose, reads their `$stable` when it initializes: kotlinc ors the fields' `$stable` together in
// the order its stability work stack pops them, and a stable, unknown or interface part drops out.
// FILE: shapes.kt
package model

class Point(val x: Int)
class Size(val width: Int)
class Color(var value: Int)
class Outer {
    class Nested(val label: String)
}
enum class Mode { ON, OFF }
object Single
sealed class Shape {
    class Leaf(var value: Int) : Shape()
}
interface Drawable
open class Opened(val x: Int)

// FILE: cards.kt
package model

import androidx.compose.runtime.BroadcastFrameClock

class Card(val point: Point)
class Mixed(val point: Point, val n: Int)
class Twice(val a: Point, val b: Point)
class Three(val a: Point, val b: Size, val c: Color)
class Unstable(val point: Point, var n: Int)
class Holder(val card: Card, val color: Color)
open class Base(val point: Point)
class Derived(val size: Size) : Base(Point(1))
class Clock(val clock: BroadcastFrameClock)
class WithInterface(val point: Point, val items: List<Int>, val drawable: Drawable)
class Kinds(val nested: Outer.Nested, val mode: Mode, val single: Single, val point: Point?)
class Sealed(val shape: Shape, val leaf: Shape.Leaf)
class HoldsOpened(val opened: Opened)

// FILE: box.kt
package model

private fun stable(name: String): Int = Class.forName(name).getField("\$stable").getInt(null)

private val expected = listOf(
    "model.Card" to 0,
    "model.Mixed" to 0,
    "model.Twice" to 0,
    "model.Three" to 8,
    "model.Unstable" to 8,
    "model.Holder" to 8,
    "model.Base" to 0,
    "model.Derived" to 0,
    "model.WithInterface" to 0,
    "model.Kinds" to 0,
    "model.Sealed" to 8,
    "model.HoldsOpened" to 8,
)

fun box(): String {
    for ((name, bits) in expected) {
        if (stable(name) != bits) return "$name: not $bits"
    }
    if (stable("model.Clock") != stable("androidx.compose.runtime.BroadcastFrameClock")) {
        return "model.Clock does not read BroadcastFrameClock"
    }
    return "OK"
}
