package refused

interface Shape {
    fun area(): Double
}

open class Base {
    override fun toString(): String = "Base"
}

class Both : Shape, Base() {
    override fun area(): Double = 1.0
}
