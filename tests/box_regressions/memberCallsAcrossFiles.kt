// FILE: EarlyCaller.kt
// Calls members of classes a later file declares: each call dispatches through the slot the
// module-wide plan numbers, whichever file is lowered first.
fun earlyBase(): String = describe(makeBase()) + describe(makeDerived())

fun describe(shape: Shape): String = shape.name() + shape.area(2) + ";"

fun earlySum(): Long = makeDerived().scaled(10L, 3)

// FILE: Shapes.kt
abstract class Shape(val label: String) {
    abstract fun area(scale: Int): Int
    open fun name(): String = label
    fun scaled(base: Long, times: Int): Long = base * times + area(1)
}

open class Square(private val side: Int) : Shape("square") {
    override fun area(scale: Int): Int = side * side * scale
}

class Tile : Square(3) {
    override fun name(): String = "tile:" + super.name()
}

fun makeBase(): Shape = Square(2)

fun makeDerived(): Square = Tile()

// FILE: LateCaller.kt
// Calls the same members after the declaring file.
fun lateCalls(square: Square): String = square.name() + square.area(1)

fun box(): String {
    if (earlyBase() != "square8;tile:square18;") return "Fail early: " + earlyBase()
    if (earlySum() != 39L) return "Fail sum: " + earlySum()
    if (lateCalls(makeDerived()) != "tile:square9") return "Fail late: " + lateCalls(makeDerived())
    return "OK"
}
