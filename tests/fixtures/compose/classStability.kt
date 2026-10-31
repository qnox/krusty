// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// Every public or internal class of a Compose module gets a `$stable` field and, unless it carries
// a stable marker, `@StabilityInferred`: kotlinc infers the stability of the classes of a file
// from their backing fields, their finality and their superclasses.
// FILE: classStability.kt
package model

import androidx.compose.runtime.Immutable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.StableMarker
import androidx.compose.runtime.State

class Point(val x: Int, val y: Int)
class Counter(var count: Int)
class Late {
    lateinit var name: String
}
class Lazily {
    val value by lazy { 1 }
}
data class Size(val width: Float, val height: Double)
object Registry
data object Empty
class Outer {
    class Nested(val label: String?)
}
internal class Hidden(val flag: Boolean)
private class Secret(val code: Long)
open class Base(val id: Int)
class Derived(val extra: Char) : Base(1)
abstract class Shape
sealed class Result {
    class Done(val value: Int) : Result()
}
class OneList(val items: List<Int>)
class TwoLists(val items: List<Int>, val keys: Set<String>)
class Handlers(val click: (Int) -> Unit, val reset: () -> Unit, val done: Unit)
class Pairs(val pair: Pair<Int, String>)
class Builder(val text: StringBuilder)
class Pattern(val regex: Regex)
enum class Mode { ON, OFF }
class WithMode(val mode: Mode, val point: Point?)
class Chain(val next: Chain?)
@Stable class Observed(var value: Int)
@Immutable class Frozen(val items: List<Int>)
@StableMarker annotation class Trusted
@Trusted class Marked(var value: Int)
class HoldsMarked(val marked: Marked)
class Suspends(val block: suspend () -> Unit)
class HoldsState(val state: State<Int>)

// FILE: box.kt
package model

private val expected = listOf(
    "model.Point" to 0,
    "model.Counter" to 8,
    "model.Late" to 8,
    "model.Lazily" to 8,
    "model.Size" to 0,
    "model.Registry" to 0,
    "model.Empty" to 0,
    "model.Outer" to 0,
    "model.Outer\$Nested" to 0,
    "model.Hidden" to 0,
    "model.Base" to 8,
    "model.Derived" to 0,
    "model.Shape" to 8,
    "model.Result" to 8,
    "model.Result\$Done" to 0,
    "model.OneList" to 8,
    "model.TwoLists" to 0,
    "model.Handlers" to 0,
    "model.Pairs" to 0,
    "model.Builder" to 8,
    "model.Pattern" to 8,
    "model.WithMode" to 0,
    "model.Chain" to 8,
    "model.Observed" to 0,
    "model.Frozen" to 0,
    "model.Marked" to 0,
    "model.HoldsMarked" to 0,
    "model.Suspends" to 8,
    "model.HoldsState" to 0,
)

fun box(): String {
    for ((name, bits) in expected) {
        if (Class.forName(name).getField("\$stable").getInt(null) != bits) return "$name: not $bits"
    }
    try {
        Class.forName("model.Secret").getDeclaredField("\$stable")
        return "model.Secret has \$stable"
    } catch (e: NoSuchFieldException) {
    }
    return "OK"
}
