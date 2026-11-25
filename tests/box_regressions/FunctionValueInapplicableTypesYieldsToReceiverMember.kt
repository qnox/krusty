// A function-typed parameter whose fixed parameters cannot take a bare call's arguments is an
// inapplicable candidate: the call moves on to the next tower level instead of committing to it.

interface Ser<T> {
    fun serialize(encoder: String, value: T)
}

inline fun <T, R> Ser<T>.map(crossinline serialize: R.(R) -> T): Ser<R> {
    return object : Ser<R> {
        // The outer call takes `this@map.serialize`; the inner call takes the function value.
        override fun serialize(encoder: String, value: R) = serialize(encoder, value.let { serialize(it, it) })
    }
}

fun box(): String {
    var out = ""
    val ints = object : Ser<Int> {
        override fun serialize(encoder: String, value: Int) {
            out = encoder + value
        }
    }
    ints.map<Int, String> { it.length + this.length }.serialize("x", "ab")
    return if (out == "x4") "OK" else "Fail: $out"
}
