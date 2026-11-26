// MODULE: library
// FILE: Library.kt
package fixture

class Registry<R>(private val label: String, private val fallback: R) {
    private var count = 0

    inner class Entry(val name: String, private val value: R?) {
        val index: Int = ++count

        fun resolved(): R = value ?: fallback

        override fun toString(): String = "$label.$name#$index"
    }

    fun register(name: String, value: R?): Entry = Entry(name, value)

    fun registered(): Int = count
}

class Plain(val prefix: String) {
    inner class Part(val suffix: String) {
        fun joined(): String = prefix + suffix
    }
}

fun describe(): String {
    val registry = Registry("r", "none")
    val first = registry.register("a", "x")
    val second = registry.register("b", null)
    return "$first=${first.resolved()},$second=${second.resolved()}," + registry.registered()
}

// MODULE: consumer(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val described = describe()
    if (described != "r.a#1=x,r.b#2=none,2") return "described $described"
    val registry = Registry("n", 7)
    val entry = registry.Entry("c", null)
    if (entry.resolved() != 7 || entry.index != 1 || registry.registered() != 1) {
        return "constructed $entry ${entry.resolved()}"
    }
    val joined = Plain("O").Part("K").joined()
    return if (joined == "OK") "OK" else "joined $joined"
}
