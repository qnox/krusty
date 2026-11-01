package grouping

interface Named {
    fun first(): Int
    val name: String
    fun name(prefix: String): String
    fun other(): Int
    val first: Int
    fun name(): String
    var name2: Int
    fun first(x: Int): Int
}

class WithCtor(val key: Int, var count: Int) {
    fun count(): Int = 0
    fun key(scale: Int): Int = 0
    companion object {
        fun key(): Int = 0
    }
    class key
}

data class Pair2(val left: Int, val right: Int) {
    fun component1(x: Int): Int = x
    fun copy(): Int = 0
    fun left(): Int = 0
}

enum class Mode {
    A, B;
    val A: Int get() = 0
    fun name(x: Int): Int = x
}

object Holder {
    val x: Int = 1
    fun y() {}
    fun x(p: Int) {}
}
