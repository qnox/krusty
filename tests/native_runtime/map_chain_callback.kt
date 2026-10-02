// Kotlin's answers for `map_chain_callback.c`: a lookup, a removal, a `put` and `containsValue`
// whose first comparison clears the map -- or clears and refills it -- while the chain it walks
// has a second node. `K` and `V` are the program classes the driver stands in for: every `K` has
// hash 7, so two of them share a bucket, and the next `equals` of either runs `action` once.
var action: (() -> Unit)? = null

fun act() {
    val a = action
    action = null
    a?.invoke()
}

class K(val n: Int, val tag: String) {
    override fun equals(other: Any?): Boolean {
        act()
        return other is K && other.n == n
    }

    override fun hashCode() = 7
    override fun toString() = tag
}

class V(val n: Int, val tag: String) {
    override fun equals(other: Any?): Boolean {
        act()
        return other is V && other.n == n
    }

    override fun hashCode() = n
    override fun toString() = tag
}

fun box(): String = buildString {
    fun line(label: String, answer: Any?, map: Map<Any, Any?>) {
        appendLine("$label: $answer $map size=${map.size}")
    }
    for (kind in listOf("HashMap", "LinkedHashMap")) {
        fun fresh(): MutableMap<Any, Any?> {
            val m: MutableMap<Any, Any?> = if (kind == "HashMap") HashMap() else LinkedHashMap()
            m[K(1, "a")] = "A"
            m[K(2, "b")] = "B"
            return m
        }
        val get = fresh()
        action = { get.clear() }
        line("$kind get clearing", get[K(2, "q")], get)
        val refill = fresh()
        action = { refill.clear(); refill[K(3, "c")] = "C"; refill[K(2, "d")] = "D" }
        line("$kind get clearing and refilling", refill[K(2, "q")], refill)
        line("$kind get after refilling", refill[K(2, "q")], refill)
        val contains = fresh()
        action = { contains.clear() }
        line("$kind containsKey clearing", contains.containsKey(K(2, "q")), contains)
        val remove = fresh()
        action = { remove.clear() }
        line("$kind remove clearing", remove.remove(K(2, "q")), remove)
        val put = fresh()
        action = { put.clear() }
        line("$kind put clearing", put.put(K(3, "c"), "C"), put)
        line("$kind get after put clearing", put[K(3, "c")], put)
        val values = fresh()
        values[K(1, "a")] = V(1, "x")
        values[K(2, "b")] = V(2, "y")
        action = { values.clear() }
        line("$kind containsValue clearing", values.containsValue(V(2, "w")), values)
        val refilled = fresh()
        refilled[K(1, "a")] = V(1, "x")
        refilled[K(2, "b")] = V(2, "y")
        action = { refilled.clear(); refilled[K(4, "e")] = V(4, "z") }
        line("$kind containsValue clearing and refilling", refilled.containsValue(V(2, "w")),
            refilled)
    }
}
