// A function value whose arity no invocation form admits yields a bare call to an applicable
// member of an implicit receiver, here and from inside a local object.

interface Ser<T> {
    fun serialize(encoder: String, value: T): String
}

fun <T> Ser<T>.direct(serialize: (Int) -> String, value: T): String = serialize("d", value)

fun <T> Ser<T>.nested(serialize: (Int) -> String, value: T): String {
    val local = object {
        fun run() = serialize("n", value)
    }
    return local.run()
}

fun box(): String {
    val strings = object : Ser<String> {
        override fun serialize(encoder: String, value: String) = encoder + value
    }
    val unused: (Int) -> String = { "Fail: function value" }
    val direct = strings.direct(unused, "1")
    if (direct != "d1") return "Fail: $direct"
    val nested = strings.nested(unused, "2")
    if (nested != "n2") return "Fail: $nested"
    return "OK"
}
