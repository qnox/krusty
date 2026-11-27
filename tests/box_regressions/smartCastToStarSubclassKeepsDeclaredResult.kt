// A smart cast of `Option<Pair<String, String>>?` to the subclass `Some<*>` keeps both
// constituents. `get()` is overridden down the hierarchy, but its most specific result is the
// declaration's `Pair<String, String>`, so the call is typed by that constituent rather than by
// `Some<*>`'s star projection. A non-null `Option<Pair<String, String>>` tested `is Some` takes
// its arguments from the declaration.
interface Option<T> {
    fun get(): T
}

class Some<T>(private val value: T) : Option<T> {
    override fun get(): T = value
}

fun starred(option: Option<Pair<String, String>>?): String {
    if (option is Some<*>) {
        val x = option.get().first
        return x
    }
    return "none"
}

fun inferred(option: Option<Pair<String, String>>): String {
    if (option is Some) {
        val x = option.get().second
        return x
    }
    return "none"
}

// A member with its own type parameters keeps the result its call inferred.
interface Source<X> {
    fun <T> value(fallback: T): T
}

class Named<X> : Source<X> {
    override fun <T> value(fallback: T): T = fallback
}

fun generic(source: Source<String>?): String {
    if (source is Named<*>) {
        val x: String = source.value("!")
        return x
    }
    return "none"
}

fun box(): String {
    val read = starred(Some("O" to "x")) + inferred(Some("y" to "K")) + starred(null) +
        generic(Named<String>())
    return if (read == "OKnone!") "OK" else read
}
