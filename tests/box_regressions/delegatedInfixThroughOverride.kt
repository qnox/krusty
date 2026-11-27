interface Plus {
    infix fun plus(other: Int): Int
}

interface PlusAgain : Plus {
    override fun plus(other: Int): Int
}

class PlusImpl(private val base: Int) : PlusAgain {
    override fun plus(other: Int): Int = base + other
}

class Delegating(plus: PlusAgain) : PlusAgain by plus

fun box(): String {
    val d = Delegating(PlusImpl(40))
    return if (d.plus(2) == 42) "OK" else "fail: ${d.plus(2)}"
}
