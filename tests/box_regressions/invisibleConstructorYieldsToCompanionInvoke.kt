// A constructor the call site cannot see does not end the bare call's callable tower: the
// companion's `operator fun invoke` is a later candidate for `Symbol("x")` and wins. Inside the
// class the same spelling sees the private constructor itself.
class Symbol private constructor(name: String) {
    private val str = "Symbol($name)"
    override fun toString(): String = str

    companion object {
        operator fun invoke(name: String): Any? = Symbol(name)
    }
}

class Counter private constructor(val value: Int) {
    companion object {
        operator fun invoke(value: Int): Counter = Counter(value + 1)
    }
}

fun box(): String {
    val symbol = Symbol("x").toString()
    val counter = Counter(1).value
    return if (symbol == "Symbol(x)" && counter == 2) "OK" else "$symbol $counter"
}
