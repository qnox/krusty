// A library's classes construct their instances, store and read their properties, call their
// members and their superclass's, and override open members and properties. A program linked
// against the library's KLIB runs each class body as the library wrote it.
// MODULE: lib
// FILE: Library.kt
package fixture

open class Base(val id: Int) {
    open fun describe(): String = "base $id"
}

class Point(val x: Int, var y: Int) : Base(x) {
    constructor(both: Int) : this(both, both)

    override fun describe(): String = "point $x $y"

    fun sum(): Int = x + y

    fun moveBy(delta: Int) {
        y = y + delta
    }
}

fun origin(): Point = Point(0)

open class Measure(open val size: Int) {
    fun twice(): Int = size * 2
}

class Exact(override val size: Int) : Measure(size + 1)

open class Tag(open var text: String)

class Echo(override var text: String) : Tag(text)

// MODULE: main(lib)
// FILE: Main.kt
import fixture.*

fun box(): String {
    val point = Point(1, 2)
    if (point.sum() != 3) return "sum"
    if (point.describe() != "point 1 2") return "describe"
    if (Base(7).describe() != "base 7") return "base"
    val base: Base = point
    if (base.describe() != "point 1 2" || base.id != 1) return "override"
    point.moveBy(5)
    if (point.y != 7) return "moveBy"
    point.y = 1
    if (point.sum() != 2) return "setter"
    if (Point(4).sum() != 8) return "secondary"
    if (origin().describe() != "point 0 0") return "origin"
    val measure: Measure = Exact(4)
    if (measure.size != 4 || measure.twice() != 8) return "override val"
    val tag: Tag = Echo("a")
    tag.text = "b"
    if (tag.text != "b") return "override var"
    return "OK"
}
