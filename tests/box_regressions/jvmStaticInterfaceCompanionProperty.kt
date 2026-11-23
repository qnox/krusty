// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: `@JvmStatic` exists only on the JVM, where kotlinc makes an interface declare
// a static bridge for each accessor of its companion's property. An interface's companion stores
// its properties itself, so their accessors are compiler defaults with no function behind them; the
// interface still declares a static bridge for each.
interface Face {
    companion object {
        @JvmStatic val z: Int = 3
        @JvmStatic val c: Int get() = 4
        @JvmStatic var v: String = "v"
        @JvmStatic val n: String? = null
        @set:JvmStatic var s: List<String> = listOf()
    }
}

fun box(): String {
    Face.v = "w"
    Face.s = listOf("s")
    val value = "" + Face.z + Face.c + Face.v + Face.n + Face.s
    return if (value == "34wnull[s]") "OK" else value
}
