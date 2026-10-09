package fixture.signatures

class Outer<T> {
    fun <T> shadow(value: T): T = value
    fun keep(value: T): T = value
    var mutable: Int = 0
    val <R> R.extension: T? get() = null

    inner class Inner<U> {
        fun both(outer: T, inner: U) {}
    }

    constructor(vararg items: T)
}

/** `Outer` again with every type parameter renamed: the member ids must not change. */
class Renamed<X> {
    fun <Y> shadow(value: Y): Y = value
    fun keep(value: X): X = value
}

fun <A : Comparable<A>> bounded(first: A, second: A): A = if (first > second) first else second

context(text: String) fun withContext(count: Int): Int = text.length + count

context(text: String) val contextual: Int get() = text.length

suspend fun suspending(block: suspend (Int) -> String): String = block(1)

fun projections(numbers: Array<out Number>, sink: MutableList<in Int>, any: List<*>) {}

fun varargs(prefix: Int, vararg names: String) {}

val <T> List<T>.second: T get() = this[1]

var counter: Int = 0

enum class Color { RED, GREEN }

expect fun platform(): Int

expect class Box() {
    fun open(): Int
}
