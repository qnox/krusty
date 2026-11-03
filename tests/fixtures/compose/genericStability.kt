// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A generic class's stability depends on its type arguments: `@StabilityInferred(parameters)` has a
// bit for each type parameter a field's type is, and the bit past them when the class is stable
// whatever its arguments. A type parameter reads as stable in the class's own `$stable`. The
// annotation is not visible at run time, so the class bytes carry the masks.
// FILE: other.kt
package model

class Point(val x: Int)
class Size(val width: Int)

// FILE: generic.kt
package model

class Holder<T>(val value: T)
class Both<A, B>(val a: A, val b: B)
class Swapped<A, B>(val b: B, val a: A)
class Second<A, B>(val b: B)
class Unused<T>(val n: Int)
class Phantom<T>
class Twice<T>(val a: T, val b: T?)
class Mutable<T>(var value: T)
class Mixed<T>(val value: T, val count: Int, val items: List<Int>)
class OnlyInterface<T>(val items: List<T>)
class Bounded<T : CharSequence>(val value: T)
class InPair<A, B>(val pair: Pair<B, Int>)
class Function<T>(val make: () -> T)
class ReadsOther<T>(val point: Point, val value: T, val size: Size)
open class Opened<T>(val value: T)
open class Base(val n: Int)
class Derived<T>(val value: T) : Base(1)
class Body<T>(value: T) {
    val held: T = value
    val count: Int = 2
}

// FILE: box.kt
package model

private fun stable(name: String): Int = Class.forName(name).getField("\$stable").getInt(null)

private val expected = listOf(
    "model.Holder" to 0,
    "model.Both" to 0,
    "model.Swapped" to 0,
    "model.Second" to 0,
    "model.Unused" to 0,
    "model.Phantom" to 0,
    "model.Twice" to 0,
    "model.Mutable" to 8,
    "model.Mixed" to 0,
    "model.OnlyInterface" to 8,
    "model.Bounded" to 0,
    "model.InPair" to 0,
    "model.Function" to 0,
    "model.Opened" to 0,
    "model.Derived" to 0,
    "model.Body" to 0,
)

fun box(): String {
    for ((name, bits) in expected) {
        if (stable(name) != bits) return "$name: not $bits"
    }
    if (stable("model.ReadsOther") != (stable("model.Point") or stable("model.Size"))) {
        return "model.ReadsOther does not read Point and Size"
    }
    return "OK"
}
