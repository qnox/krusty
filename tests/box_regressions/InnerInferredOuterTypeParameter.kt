// An inner class's members see the outer class's type parameter through their implicit receiver,
// so a result inferred from the inner class's own properties keeps `T` rather than its bound.
class Counting<out T>(private val source: Iterable<T>, private val fallback: T) : Iterable<T> {
    inner class Walk(private val walked: Iterator<T>) : Iterator<T> {
        override fun hasNext() = walked.hasNext()
        override fun next() = walked.next()
        fun walked() = walked
        fun orFallback() = if (walked.hasNext()) walked.next() else fallback
    }

    override fun iterator() = Walk(source.iterator())

    fun first(): T = iterator().orFallback()
    fun iteratorOf(): Iterator<T> = iterator().walked()
}

class Two(private val first: String, private val second: String) : Iterable<String> {
    override fun iterator(): Iterator<String> = object : Iterator<String> {
        private var index = 0
        override fun hasNext() = index < 2
        override fun next() = if (index++ == 0) first else second
    }
}

fun box(): String {
    val counting = Counting(Two("O", "K"), "?")
    var joined = ""
    for (part in counting) joined += part
    if (joined != "OK") return "joined: $joined"
    if (counting.first() != "O") return "first: ${counting.first()}"
    val drained = counting.iterator()
    drained.next()
    drained.next()
    if (drained.orFallback() != "?") return "fallback"
    return counting.iteratorOf().next() + "K"
}
