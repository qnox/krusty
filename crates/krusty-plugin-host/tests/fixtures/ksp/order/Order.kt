package order

class First

fun afterClass() {}

class Mixed(val a: Int) {
    constructor() : this(0)
    class Nested
    fun f() {}
    val p: Int = 1
    object Obj
    fun g() {}
    var q: Int = 2
    init {}
    enum class E { X, Y }
}

val last: Int = 0

typealias Alias = Int

enum class Color(val rgb: Int) {
    RED(1), GREEN(2);
    fun hex(): String = ""
}
