// FILE: EarlyMaker.kt
// Constructs classes a later file declares, through their primary and secondary constructors.
fun early(): String {
    val point = Point(1, 2)
    val labelled = Labelled("p", point)
    val copy = Labelled(labelled)
    return point.sum().toString() + labelled.describe() + copy.describe()
}

// FILE: Shapes.kt
open class Point(val x: Int, val y: Int) {
    open fun sum(): Int = x + y
}

class Labelled(val label: String, val point: Point) {
    constructor(other: Labelled) : this(other.label + "'", Point(other.point.x * 10, other.point.y))

    fun describe(): String = "[" + label + ":" + point.sum() + "]"
}

// FILE: LateMaker.kt
fun box(): String {
    val result = early()
    if (result != "3[p:3][p':12]") return "Fail: " + result
    if (Point(4, 5).sum() != 9) return "Fail late"
    return "OK"
}
