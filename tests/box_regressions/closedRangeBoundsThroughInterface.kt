// A ClosedRange<Int> that holds the program's own class answers its bounds from that class's
// getters, and one that holds a range the runtime builds answers them as before.
class Window(private val low: Int, private val high: Int) : ClosedRange<Int> {
    override val start: Int
        get() = low
    override val endInclusive: Int
        get() = high
}

fun bounds(range: ClosedRange<Int>): String = "${range.start}..${range.endInclusive}"

fun box(): String {
    val own: ClosedRange<Int> = Window(2, 5)
    if (own.start != 2) return "own start ${own.start}"
    if (own.endInclusive != 5) return "own end ${own.endInclusive}"
    if (bounds(own) != "2..5") return "own bounds ${bounds(own)}"
    val built: ClosedRange<Int> = 3..7
    if (built.start != 3) return "built start ${built.start}"
    if (built.endInclusive != 7) return "built end ${built.endInclusive}"
    if (bounds(3..7) != "3..7") return "built bounds ${bounds(3..7)}"
    val window = Window(8, 9)
    if (window.start != 8 || window.endInclusive != 9) return "direct ${window.start}..${window.endInclusive}"
    return "OK"
}
