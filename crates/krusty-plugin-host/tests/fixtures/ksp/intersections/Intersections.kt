package intersections

interface Shape {
    fun area(): Double
}

open class Base {
    override fun toString(): String = "Base"
}

// `toString` meets kotlin.Any's (through Shape) and Base's override of it.
class Both : Shape, Base() {
    override fun area(): Double = 1.0
}

class After : Base(), Shape {
    override fun area(): Double = 1.0
}

interface Left {
    @Deprecated("left")
    fun shared(x: Int): Int
    fun same(): String
    @Deprecated("label")
    val label: String
    val count: Int
    var both: Int
}

interface Right {
    fun shared(y: Int): Int
    fun shared(y: String): Int
    fun same(): String
    val label: String
    val other: Int
    var both: Int
}

// Declared before the classifier its intersection members are made for.
abstract class Further : Joined()

abstract class Joined : Left, Right

open class Impl {
    open fun shared(z: Int): Int = z
    open val label: String = ""
}

abstract class InterfaceFirst : Left, Impl()

abstract class ClassFirst : Impl(), Left

open class Closed {
    fun shared(w: Int): Int = w
}

abstract class ClosedSecond : Left, Closed()

interface Root {
    fun root(): Int
}

interface First : Root {
    override fun root(): Int
}

interface Second : Root

abstract class Sides : First, Second

abstract class Three : Left, Right, Impl()
