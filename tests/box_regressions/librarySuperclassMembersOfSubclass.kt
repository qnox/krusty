// A class extending a library class runs the library's members on its own instances: the
// library's final `next` calls the subclass's `nextInt` through the class's dispatch table.

class Counter(private val end: Int) : IntIterator() {
    private var i = 0
    override fun hasNext() = i < end
    override fun nextInt() = i++
}

class Letters : CharIterator() {
    private var i = 0
    override fun hasNext() = i < 2
    override fun nextChar() = "OK"[i++]
}

fun box(): String {
    val ints: IntIterator = Counter(3)
    var sum = 0
    while (ints.hasNext()) sum += ints.next()
    if (sum != 3) return "IntIterator: $sum"

    val chars: CharIterator = Letters()
    return "" + chars.next() + chars.next()
}
