class Window(private val from: Int, private val to: Int) : ClosedRange<Int> {
    override val start: Int
        get() = from
    override val endInclusive: Int
        get() = to
}

class Everything() : ClosedRange<Int> {
    override val start: Int
        get() = 0
    override val endInclusive: Int
        get() = 0
    override fun contains(value: Int) = true
}

fun box(): String {
    val window = Window(2, 5)
    if (3 !in window) return "fail contains inside"
    if (6 in window) return "fail contains outside"
    if (window.isEmpty()) return "fail non-empty"
    if (!Window(5, 2).isEmpty()) return "fail empty"
    if (1 !in Everything()) return "fail override"
    return "OK"
}
