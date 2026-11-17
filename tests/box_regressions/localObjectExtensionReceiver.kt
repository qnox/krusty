// An object expression whose member reads the extension receiver of the function it is written
// in, of a top-level function and of a local one.
class Countdown(private val from: Int) : Iterable<Int> {
    override fun iterator(): Iterator<Int> = object : Iterator<Int> {
        var next = from
        override fun hasNext(): Boolean = next > 0
        override fun next(): Int = next--
    }
}

fun <T> Iterable<T>.wrapped(): Iterable<T> = object : Iterable<T> {
    override fun iterator(): Iterator<T> = this@wrapped.iterator()
}

fun box(): String {
    var digits = ""
    for (x in Countdown(3).wrapped()) digits += x
    if (digits != "321") return "Fail wrapped: $digits"
    fun String.framed() = object {
        override fun toString(): String = "[" + this@framed + "]"
    }
    val framed = "OK".framed().toString()
    if (framed != "[OK]") return "Fail framed: $framed"
    return "OK"
}
