interface Node {
    fun id(): String
}

interface Styled : Node

interface Backed : Node

class Paragraph : Styled, Backed {
    override fun id() = "P"
}

class Link : Styled, Backed {
    override fun id() = "L"
}

class Items : Node {
    override fun id() = "I"
}

class Pair2<T>(val first: T?, val second: T?)

fun <T> pairOf(first: T?, second: T?): Pair2<T> = Pair2(first, second)

fun <R> compute(block: () -> R): R = block()

fun <R> choose(first: () -> R, second: () -> R): R = second()

fun <T, R> T.map(block: (T) -> R): R = block(this)

fun render(pair: Pair2<Node>): String = (pair.first?.id() ?: "-") + (pair.second?.id() ?: "-")

fun box(): String {
    val ids0 = true
    fun content(wrap: Boolean) =
        pairOf(
            Link().map {
                if (wrap) Paragraph() else it
            },
            Items(),
        )

    fun joined(wrap: Boolean) = pairOf(compute { if (wrap) Paragraph() else Link() }, Items())

    // A sibling lambda or an early return that is not a Node keeps the variable denotable.
    val mixed: Any = choose({ if (ids0) Paragraph() else Link() }, { "text" })
    val early: Any = compute {
        if (ids0) return@compute "early"
        if (ids0) Paragraph() else Link()
    }

    val ids = render(content(true)) + render(content(false))
    val joined = render(joined(false))
    return if (ids == "PILI" && joined == "LI" && mixed == "text" && early == "early") "OK" else "Fail: $ids $joined"
}
