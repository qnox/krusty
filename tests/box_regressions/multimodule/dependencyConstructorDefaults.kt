// MODULE: library
// FILE: Library.kt
package fixture

open class Outcome(
    val cause: String,
    handled: Boolean = false,
    val weight: Int = if (handled) 1 else 2,
) {
    private var flag = handled
    val handled: Boolean get() = flag

    fun markHandled(): Boolean {
        val was = flag
        flag = true
        return !was
    }

    override fun toString(): String = "$cause:$flag:$weight"
}

class Failed(cause: String) : Outcome(cause, handled = true)

class Span {
    val start: Int
    val end: Int

    constructor(start: Int, end: Int = start + 10) {
        this.start = start
        this.end = end
    }

    override fun toString(): String = "$start..$end"
}

class Tag(val name: String, val level: Int = 1) {
    constructor(flag: Boolean) : this(if (flag) "on" else "off")

    override fun toString(): String = "$name$level"
}

class Label(val text: String) {
    constructor(count: Int, suffix: String = "x") : this("$count$suffix")

    override fun toString(): String = text
}

fun describe(cause: String): String {
    val first = Outcome(cause)
    val marked = first.markHandled()
    val second = Outcome(cause, true)
    val third = Outcome(cause, weight = 7)
    val failed = Failed(cause)
    val spans = listOf(Span(3), Span(1, 2)).joinToString(" ")
    val labels = listOf(Label(4), Label(5, "y"), Tag(true), Tag("t", 3)).joinToString(" ")
    return "$first $marked ${second.markHandled()} $third $failed $spans $labels"
}

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val described = describe("ab")
    val expected = "ab:true:2 true false ab:false:7 ab:true:1 3..13 1..2 4x 5y on1 t3"
    return if (described == expected) "OK" else described
}
