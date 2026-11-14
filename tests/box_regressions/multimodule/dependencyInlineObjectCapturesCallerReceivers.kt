// A lambda passed to a dependency's inline function, whose object invokes it, captures the
// caller's receivers: the enclosing instance of a member, and the extension receiver of the
// function it is written in. The object the call copies stores each of them next to its own
// captures, and reads the caller's properties through their accessors.
// WITH_STDLIB
// MODULE: lib
// FILE: Library.kt
package fixture

interface Keyed {
    fun key(value: Int): Int
}

inline fun byKey(crossinline key: (Int) -> Int): Keyed =
    object : Keyed {
        override fun key(value: Int): Int = key(value)
    }

inline fun <T> later(crossinline produce: () -> T): () -> T = { produce() }

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

class Scale(val factor: Int) {
    var offset = 1

    fun remainder(): Keyed = byKey { it % factor + offset }

    fun Int.distance(): Keyed = byKey { if (it < this) this - it else it - this }

    fun distanceFromSix(): Keyed = 6.distance()

    fun String.prefixed(): Keyed = byKey { if (it < length) factor else offset }

    fun prefixedBy(prefix: String): Keyed = prefix.prefixed()

    fun deferred(): () -> Int = later { factor * 10 + offset }
}

fun box(): String {
    val scale = Scale(3)
    val remainder = scale.remainder()
    if (remainder.key(5) != 3) return "remainder: ${remainder.key(5)}"
    scale.offset = 4
    if (remainder.key(7) != 5) return "remainder after write: ${remainder.key(7)}"
    val distance = scale.distanceFromSix()
    if (distance.key(1) != 5 || distance.key(9) != 3) return "distance"
    val prefixed = scale.prefixedBy("ab")
    if (prefixed.key(1) != 3 || prefixed.key(2) != 4) return "prefixed"
    if (scale.deferred()() != 34) return "deferred: ${scale.deferred()()}"
    return "OK"
}
