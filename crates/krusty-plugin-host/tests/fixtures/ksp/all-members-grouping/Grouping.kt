package grouping

interface Named {
    fun first(): Int
    val name: String
    fun name(prefix: String): String
    fun other(): Int
    val first: Int
    fun name(): String
}

abstract class Base {
    fun base(): Int = 0
    abstract val base: Int
    fun shared(): Int = 0
}

abstract class Derived : Base(), Named {
    fun own(): Int = 0
    override fun name(): String = ""
    override val first: Int = 0
    fun shared(scale: Int): Int = scale
}

data class Pair2(val left: Int, val right: Int) {
    fun component1(x: Int): Int = x
    fun copy(): Int = 0
    fun left(): Int = 0
}
