interface PublishedReader<T> {
    fun read(): T
    fun label(): String
}

fun unrelatedPublishedInput(first: Int, second: String, third: Boolean): String = second

inline fun <T> publishedReader(crossinline action: () -> T, suffix: String): PublishedReader<T> {
    val label = suffix
    return object : PublishedReader<T> {
        override fun read(): T = action()
        override fun label(): String = label
    }
}

fun box(): String {
    var effects = 0
    val reader = publishedReader({ effects++; "OK" }, "kept")
    if (reader.label() != "kept" || effects != 0) return "FAIL before"
    val result = reader.read()
    if (effects != 1) return "FAIL effects"
    return result
}
