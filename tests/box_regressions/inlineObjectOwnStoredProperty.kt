// Each call of `wrap` regenerates its object. The copy reads and writes its own stored
// properties in its initializer and methods, so every backend reaches the copy's storage.
inline fun wrap(crossinline text: () -> String) =
    object {
        val value = text()
        var count = 0

        init {
            count = value.length
        }

        fun grow(): Int {
            count += 1
            return count
        }

        override fun toString() = value + grow()
    }

fun box(): String {
    val result = wrap { "O" }.toString() + wrap { "KK" }.toString()
    return if (result == "O2KK3") "OK" else result
}
