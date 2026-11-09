// The stdlib's `Iterable { ... }` and `Sequence { ... }` are inline factories whose body is an
// object expression capturing a crossinline lambda. The lambda captures a local of the caller, and
// each value it produces is read back through the object.
class Countdown(val left: Int, val step: Int) : Iterator<Int> {
    override fun hasNext(): Boolean = false
    override fun next(): Int = throw NoSuchElementException()
}

fun box(): String {
    val step = 10
    val numbers = Iterable { Countdown(3, step) }
    val first = numbers.iterator() as Countdown
    if (first.left != 3 || first.step != 10) return "FAIL iterable ${first.left} ${first.step}"
    if (numbers.iterator() === first) return "FAIL fresh iterator"
    val doubled = Sequence { Countdown(2, step * 2) }
    val second = doubled.iterator() as Countdown
    if (second.left != 2 || second.step != 20) return "FAIL sequence ${second.left} ${second.step}"
    return "OK"
}
