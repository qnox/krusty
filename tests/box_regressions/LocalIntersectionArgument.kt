interface Node

interface Styled : Node

interface Backed : Node

class Paragraph : Styled, Backed

class Link : Styled, Backed

class Items : Node

class Pair2<T>(val first: T?, val second: T?)

class Bag<T>(val count: Int)

fun <T> pairOf(first: T?, second: T?): Pair2<T> = Pair2(first, second)

fun <T> bagOf(element: T): Bag<T> = Bag(1)

fun <T> bagOf(vararg elements: T): Bag<T> = Bag(elements.size)

fun renderPair(pair: Pair2<Node>): Int = if (pair.first != null) 1 else 0

fun renderBag(bag: Bag<Node>): Int = bag.count

fun pairFromInitializer(wrap: Boolean): Int {
    val x = if (wrap) Paragraph() else Link()
    return renderPair(pairOf(x, Items()))
}

fun pairFromNarrowing(value: Any): Int {
    if (value is Styled && value is Backed) return renderPair(pairOf(value, Items()))
    return 0
}

fun bagFromInitializer(wrap: Boolean): Int {
    val x = if (wrap) Paragraph() else Link()
    return renderBag(bagOf(x, Items()))
}

fun bagFromNarrowing(value: Any): Int {
    if (value is Styled && value is Backed) return renderBag(bagOf(value, Items()))
    return 0
}

fun singleFromNarrowing(value: Any): Int {
    if (value is Styled && value is Backed) return renderBag(bagOf(value))
    return 0
}

fun box(): String {
    val pairs = pairFromInitializer(true) + pairFromNarrowing(Paragraph())
    val bags = bagFromInitializer(true) + bagFromNarrowing(Paragraph())
    val single = singleFromNarrowing(Paragraph())
    return if (pairs == 2 && bags == 4 && single == 1) "OK" else "Fail: $pairs $bags $single"
}
