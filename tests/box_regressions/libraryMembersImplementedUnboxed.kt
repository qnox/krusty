// A class implementing a library interface member whose type parameter it fixes to a primitive
// carries the value unboxed (`next(): Int` implementing `Iterator<T>.next(): T`). A call through
// the library member, from source and from a library body, boxes the result and unboxes an
// argument (`compare(a: Long, b: Long)` implementing `Comparator<T>.compare`).
class Counter(private val last: Int) : Iterator<Int> {
    private var next = 1
    override fun hasNext(): Boolean = next <= last
    override fun next(): Int = next++
}

class Numbers(private val last: Int) : Iterable<Int> {
    override fun iterator(): Iterator<Int> = Counter(last)
}

class Descending : Comparator<Long> {
    override fun compare(a: Long, b: Long): Int = b.compareTo(a)
}

fun sum(values: Iterable<Int>): Int {
    var total = 0
    for (value in values) total += value
    return total
}

fun <T> compare(comparator: Comparator<T>, left: T, right: T): Int =
    comparator.compare(left, right)

fun box(): String {
    if (sum(Numbers(4)) != 10) return "fail: sum ${sum(Numbers(4))}"
    val iterator: Iterator<Any> = Counter(1)
    if (iterator.next() != 1) return "fail: boxed next"
    if (Numbers(3).firstOrNull() != 1) return "fail: firstOrNull"
    if (compare(Descending(), 5L, 3L) >= 0) return "fail: compare greater"
    if (compare(Descending(), 5L, 7L) <= 0) return "fail: compare less"
    return "OK"
}
