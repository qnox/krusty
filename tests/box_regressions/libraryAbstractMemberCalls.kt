// A call of a library class's abstract member runs the override of the receiver's class, the most
// derived one where a subclass overrides it again.

open class Counter(private val end: Int) : IntIterator() {
    protected var i = 0
    override fun hasNext() = i < end
    override fun nextInt() = i++
}

class Doubled(end: Int) : Counter(end) {
    override fun nextInt() = 2 * i++
}

class Letters : CharIterator() {
    private var i = 0
    override fun hasNext() = i < 2
    override fun nextChar() = "OK"[i++]
}

fun sum(ints: IntIterator): Int {
    var sum = 0
    while (ints.hasNext()) sum += ints.nextInt()
    return sum
}

fun box(): String {
    if (sum(Counter(3)) != 3) return "Counter"
    if (sum(Doubled(3)) != 6) return "Doubled"
    val chars: CharIterator = Letters()
    return "" + chars.nextChar() + chars.nextChar()
}
