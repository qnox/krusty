// WITH_STDLIB
// `asSequence` returns the object its inlined `Sequence { }` builds. The library types that object
// with the erasure of `Sequence`'s type parameter, and casts it back to the declared `Sequence<T>`.
class Countdown(private val from: Int) : Iterable<Int> {
    override fun iterator(): Iterator<Int> = object : Iterator<Int> {
        var next = from
        override fun hasNext(): Boolean = next > 0
        override fun next(): Int = next--
    }
}

fun box(): String {
    var digits = ""
    for (x in Countdown(3).asSequence()) digits += x
    if (digits != "321") return "Fail asSequence: $digits"
    var indexed = ""
    for ((index, value) in Countdown(2).asSequence().withIndex()) indexed += "$index:$value "
    if (indexed != "0:2 1:1 ") return "Fail withIndex: $indexed"
    return "OK"
}
