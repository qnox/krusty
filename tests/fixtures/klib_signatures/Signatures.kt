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

var guarded: Int = 0
    private set

enum class Color { RED, GREEN }

expect fun platform(): Int

expect class Box() {
    fun open(): Int
}

/** A companion-block member signs `#static`; a companion extension signs `#companion@` and its class. */
class Registry {
    companion {
        fun create(): Registry = Registry()
        val size: Int get() = 0
        var label: String = ""
    }
}

companion fun Registry.named(name: String): Registry = Registry()

companion val Registry.capacity: Int get() = 1

/** A classifier hierarchy: every kind, modality, and member shape the provider publishes. */
interface Shape {
    val area: Int
    fun describe(): String = "shape"
}

fun interface Action {
    fun run(value: Int): Int
}

annotation class Marker(val level: Int)

abstract class Base<T : CharSequence>(val label: T) : Shape {
    protected abstract fun hidden(): Int
    internal open fun tuned(): Int = 0
    private fun secret(): Int = 1
    fun Int.scaled(): Int = this * 2
}

open class Derived(label: String) : Base<String>(label), Comparable<Derived> {
    constructor(count: Int) : this(count.toString())

    override val area: Int get() = 1
    override fun hidden(): Int = 2
    override fun compareTo(other: Derived): Int = 0
    suspend fun fetch(): Int = 0

    class Nested(val depth: Int)

    companion object Factory {
        fun create(): Derived = Derived("x")
    }
}

object Singleton {
    const val LIMIT: Int = 3
    fun ping(): Int = LIMIT
}
