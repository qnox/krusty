// A nested call whose lambda result is already fixed by its inputs (`value.let { f(it) }` returns
// `T`) does not fit a parameter of a different type parameter `R`: the anonymous object's member is
// inapplicable and the extension receiver's member is called.

interface Ser<T> {
    fun serialize(encoder: String, value: T)
}

fun <T, R> Ser<T>.map(f: (R) -> T): Ser<R> = object : Ser<R> {
    override fun serialize(encoder: String, value: R): Unit = serialize(encoder, value.let { f(it) })
}

fun box(): String {
    var out = ""
    val ints = object : Ser<Int> {
        override fun serialize(encoder: String, value: Int) {
            out = encoder + value
        }
    }
    ints.map<Int, String> { it.length }.serialize("x", "abc")
    return if (out == "x3") "OK" else "Fail: $out"
}
