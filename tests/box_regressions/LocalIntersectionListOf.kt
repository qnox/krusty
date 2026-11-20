interface Node

interface Styled : Node

interface Backed : Node

class Paragraph : Styled, Backed

class Link : Styled, Backed

class Items : Node

fun render(nodes: List<Node>): Int = nodes.size

fun fromInitializer(wrap: Boolean): Int {
    val x = if (wrap) Paragraph() else Link()
    return render(listOf(x, Items()))
}

fun fromNarrowing(value: Any): Int {
    if (value is Styled && value is Backed) return render(listOf(value, Items()))
    return 0
}

fun single(value: Any): Int {
    if (value is Styled && value is Backed) return render(listOf(value))
    return 0
}

fun box(): String {
    val initializer = fromInitializer(true)
    val narrowing = fromNarrowing(Paragraph())
    val single = single(Paragraph())
    return if (initializer == 2 && narrowing == 2 && single == 1) "OK" else "Fail: $initializer $narrowing $single"
}
