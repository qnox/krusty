// FILE: lib.kt
inline fun reversedOrder(name: String): Comparator<String> =
    object : Comparator<String> {
        override fun compare(a: String, b: String): Int = b.compareTo(a)
        override fun toString(): String = "reversed $name"
    }

fun describe(value: Any): String = value.toString()

// FILE: main.kt
fun box(): String {
    val order = reversedOrder("names")
    if (order.compare("a", "c") <= 0) return "compare: ${order.compare("a", "c")}"
    val described = describe(order)
    if (described != "reversed names") return "toString: $described"
    return "OK"
}
